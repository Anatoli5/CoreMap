// Вызовы Windows: привязка потоков к ядрам, запрет сна, работа с окнами.
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Threading;

namespace CoreMap
{
    static class Native
    {
        [DllImport("kernel32.dll")] static extern IntPtr GetCurrentThread();
        [DllImport("kernel32.dll")] static extern UIntPtr SetThreadAffinityMask(IntPtr thread, UIntPtr mask);
        [DllImport("kernel32.dll")] static extern bool SetThreadPriority(IntPtr thread, int priority);
        [DllImport("kernel32.dll")] static extern uint SetThreadExecutionState(uint flags);
        [DllImport("kernel32.dll", SetLastError = true)]
        public static extern bool GetLogicalProcessorInformationEx(int relation, IntPtr buffer, ref int length);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr LoadLibrary(string path);
        [DllImport("user32.dll")] static extern bool FlashWindow(IntPtr window, bool invert);
        [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr window);
        [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr window, int command);

        /// Привязывает текущий поток к одному логическому процессору.
        /// Привязывает текущий поток к логическому процессору. false — не удалось (например, процессу запрещено это ядро):
        /// тогда чтение и запись «по ядру» попали бы на какое-то другое ядро.
        public static bool Pin(int lp)
        {
            return SetThreadAffinityMask(GetCurrentThread(), (UIntPtr)(1UL << lp)) != UIntPtr.Zero;
        }

        /// Секунды по монотонным часам: для интервалов, не зависит от перевода системного времени.
        public static double Now()
        {
            return (double)Stopwatch.GetTimestamp() / Stopwatch.Frequency;
        }

        /// Разрешает текущему потоку работать на всех перечисленных процессорах.
        public static void Unpin(int[] lps)
        {
            ulong mask = 0;
            foreach (int lp in lps) mask |= 1UL << lp;
            SetThreadAffinityMask(GetCurrentThread(), (UIntPtr)mask);
        }

        public static void RaiseThreadPriority() { SetThreadPriority(GetCurrentThread(), 2); }
        public static void LowerThreadPriority() { SetThreadPriority(GetCurrentThread(), -1); }

        /// Пока идёт измерение, компьютер не уходит в сон: иначе получасовой прогон оборвётся.
        public static void KeepAwake(bool on)
        {
            const uint Continuous = 0x80000000, SystemRequired = 0x00000001;
            SetThreadExecutionState(Continuous | (on ? SystemRequired : 0));
        }

        public static void Flash(IntPtr window) { FlashWindow(window, true); }

        /// Показывает окно уже запущенной копии программы.
        public static void ActivateOtherCopy()
        {
            Process me = Process.GetCurrentProcess();
            foreach (Process p in Process.GetProcessesByName(me.ProcessName))
            {
                if (p.Id == me.Id || p.MainWindowHandle == IntPtr.Zero) continue;
                ShowWindow(p.MainWindowHandle, 9);  // SW_RESTORE
                SetForegroundWindow(p.MainWindowHandle);
            }
        }
    }
}
