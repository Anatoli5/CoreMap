# Core-to-core latency: time to hand one cache line from core A to core B.
# Two threads pinned to A and B bounce a single memory cell back and forth;
# the result is the one-way time in nanoseconds.
#
# The time depends on WHICH cache line is bounced, not only on the pair of cores: measured on a 275HX,
# the same pair gives 28 to 43 ns for different lines, and with one line some core looks "fast"
# with everybody. So every pair is measured on many lines spread over a large array, and the
# output is the mean, the lowest and the highest line: "A B mean min max" per pair.
# -PerLine prints every line's value instead.
# ASCII only: Windows PowerShell 5.1 reads a BOM-less script in the ANSI code page.
param(
    [string]$Cpus = "",
    [int]$Lines = 48,
    [int]$Iters = 3000,
    [int]$Samples = 2,
    [switch]$PerLine
)

$src = @'
using System;
using System.Diagnostics;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Threading;

public static class CoreLatency {
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentThread();
    [DllImport("kernel32.dll")] static extern UIntPtr SetThreadAffinityMask(IntPtr thread, UIntPtr mask);

    const long PING = 1, PONG = 2, STOP = 3;
    const int LINE = 8;                          // longs per 64-byte cache line
    const int TOTAL_LINES = 8192;                // 512 KB
    static long[] mem = new long[TOTAL_LINES * LINE];

    static void Pin(int cpu) {
        Thread.BeginThreadAffinity();
        SetThreadAffinityMask(GetCurrentThread(), (UIntPtr)(1UL << cpu));
    }

    static void RoundTrips(int cell, int n) {
        for (int i = 0; i < n; i++)
            while (Interlocked.CompareExchange(ref mem[cell], PING, PONG) != PONG) { }
    }

    // One line, the calling thread already pinned to core A. Best of several samples.
    static double MeasureLine(int cell, int b, int iters, int samples, int warmMs) {
        mem[cell] = PONG;
        var ready = new ManualResetEventSlim(false);
        var responder = new Thread(() => {
            Pin(b);
            ready.Set();
            while (Interlocked.CompareExchange(ref mem[cell], PONG, PING) != STOP) { }
        });
        responder.IsBackground = true;
        responder.Start();
        ready.Wait();

        // Warm-up: the cores leave idle and ramp their frequency.
        var warm = Stopwatch.StartNew();
        while (warm.ElapsedMilliseconds < warmMs) RoundTrips(cell, 1000);
        RoundTrips(cell, 500);

        double best = double.MaxValue;
        for (int s = 0; s < samples; s++) {
            long t0 = Stopwatch.GetTimestamp();
            RoundTrips(cell, iters);
            long t1 = Stopwatch.GetTimestamp();
            double ns = (t1 - t0) * 1e9 / Stopwatch.Frequency / iters / 2;
            if (ns < best) best = ns;
        }
        while (Interlocked.Read(ref mem[cell]) != PONG) { }
        Interlocked.Exchange(ref mem[cell], STOP);
        responder.Join();
        return best;
    }

    static string F(double v) { return v.ToString("F1", CultureInfo.InvariantCulture); }

    public static void Run(int[] cpus, int lines, int iters, int samples, bool perLine) {
        for (int i = 0; i < cpus.Length; i++)
            for (int j = i + 1; j < cpus.Length; j++) {
                Pin(cpus[i]);
                double sum = 0, lo = double.MaxValue, hi = 0;
                string all = "";
                for (int k = 0; k < lines; k++) {
                    // 521 is coprime with the line count: the lines are spread over the whole array.
                    int cell = (k * 521 + 37) % TOTAL_LINES * LINE;
                    double ns = MeasureLine(cell, cpus[j], iters, samples, k == 0 ? 10 : 0);
                    sum += ns; if (ns < lo) lo = ns; if (ns > hi) hi = ns;
                    all += " " + F(ns);
                }
                Console.WriteLine(cpus[i] + " " + cpus[j] + (perLine ? all : " " + F(sum / lines) + " " + F(lo) + " " + F(hi)));
                Console.Out.Flush();
            }
    }
}
'@

Add-Type -TypeDefinition $src
if ($Cpus) { $list = [int[]]($Cpus -split ',') } else { $list = [int[]](0..([Environment]::ProcessorCount - 1)) }
[CoreLatency]::Run($list, $Lines, $Iters, $Samples, $PerLine.IsPresent)
