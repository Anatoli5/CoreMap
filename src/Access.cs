// Доступ к каналу моста: кто может его открыть и кто на другом конце.
//
// Канал создаёт процесс с правами администратора, открывает — окно без них. Поэтому:
// • Канал создаётся прямым вызовом Windows, а не классом .NET: тому нельзя сказать «это первый и единственный
//   канал с таким именем» (иначе имя заранее может занять чужая программа) и «никого по сети». Со списком доступа
//   из .NET тот же пользователь входил через сетевой путь этой же машины даже при записи «сети — отказ»; закрыл
//   это только признак PIPE_REJECT_REMOTE_CLIENTS (проба 13.10.2026, outputs\bridge-probe-2026-10-13\report.md).
// • Пускается пользователь сеанса Windows, а не тот, от чьего имени работает сам процесс: при входе с правами
//   по чужому паролю и при включённой «защите администратора» Windows это разные учётные записи.
// • Право создавать новые экземпляры канала пользователю не даётся (0x12019b = чтение, запись, ожидание —
//   без FILE_CREATE_PIPE_INSTANCE).
// • Метка целостности «средняя» ставится явно: по описанию Windows она такая и без записи, но от умолчания
//   результат зависеть не должен. Из процесса с правами это не проверено — проверяет владелец кнопкой (BridgeCheck.cs).
using System;
using System.ComponentModel;
using System.IO.Pipes;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace CoreMap
{
    static class Access
    {
        public const string Medium = "S-1-16-8192", High = "S-1-16-12288";  // уровни целостности: без прав администратора и с ними

        /// Описание доступа к каналу: сети — отказ, пользователю admit — чтение и запись, системе и администраторам — всё.
        public static string Sddl(SecurityIdentifier admit)
        {
            return "D:P(D;;FA;;;NU)(A;;0x12019b;;;" + admit.Value + ")(A;;FA;;;SY)(A;;FA;;;BA)S:(ML;;NW;;;ME)";
        }

        /// Создаёт канал: единственный с таким именем, один экземпляр, только для этой машины.
        /// Имя занято (или канал уже есть) — ошибка Windows: мост не начинает работу на чужом канале.
        public static NamedPipeServerStream Create(string name, SecurityIdentifier admit)
        {
            const uint Duplex = 0x3, FirstInstance = 0x00080000, Overlapped = 0x40000000, RejectRemote = 0x8;
            IntPtr descriptor;
            uint size;
            if (!ConvertStringSecurityDescriptorToSecurityDescriptor(Sddl(admit), 1, out descriptor, out size)) throw new Win32Exception();
            try
            {
                var attributes = new SecurityAttributes { Length = Marshal.SizeOf(typeof(SecurityAttributes)), Descriptor = descriptor };
                SafePipeHandle handle = CreateNamedPipe("\\\\.\\pipe\\" + name, Duplex | FirstInstance | Overlapped, RejectRemote, 1,
                    65536, 4096, 0, ref attributes);
                if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
                return new NamedPipeServerStream(PipeDirection.InOut, true, false, handle);
            }
            finally { LocalFree(descriptor); }
        }

        /// Владелец, список доступа и метка целостности канала — текстом, как их видит эта сторона.
        public static string Describe(SafePipeHandle handle)
        {
            const uint Owner = 0x1, Dacl = 0x4, Label = 0x10;
            string text = "";
            foreach (uint part in new[] { Owner | Dacl, Label })
            {
                IntPtr descriptor, owner, group, dacl, sacl;
                uint error = GetSecurityInfo(handle, 6 /* SE_KERNEL_OBJECT */, part, out owner, out group, out dacl, out sacl, out descriptor);
                if (error != 0) { text += " [" + (part == Label ? "метка" : "владелец и доступ") + ": ошибка " + error + "]"; continue; }
                try
                {
                    IntPtr sddl;
                    uint length;
                    if (!ConvertSecurityDescriptorToStringSecurityDescriptor(descriptor, 1, part, out sddl, out length)) continue;
                    string piece = Marshal.PtrToStringUni(sddl);
                    text += part == Label && piece == "" ? " [метки нет]" : piece;
                    LocalFree(sddl);
                }
                finally { LocalFree(descriptor); }
            }
            return text;
        }

        /// Обрывает все чтения и записи, ожидающие на канале, какой бы поток их ни начал.
        public static void CancelAll(SafePipeHandle pipe) { CancelIoEx(pipe, IntPtr.Zero); }

        public static uint ClientProcess(SafePipeHandle pipe)
        {
            uint id;
            return GetNamedPipeClientProcessId(pipe, out id) ? id : 0;
        }

        public static uint ServerProcess(SafePipeHandle pipe)
        {
            uint id;
            return GetNamedPipeServerProcessId(pipe, out id) ? id : 0;
        }

        /// Полный путь exe процесса; null — процесс не открылся даже для такого вопроса.
        public static string ImagePath(uint process)
        {
            IntPtr handle = OpenProcess(QueryLimited, false, process);
            if (handle == IntPtr.Zero) return null;
            try
            {
                var path = new StringBuilder(1024);
                uint size = (uint)path.Capacity;
                return QueryFullProcessImageName(handle, 0, path, ref size) ? path.ToString() : null;
            }
            finally { CloseHandle(handle); }
        }

        /// Есть ли сейчас канал с таким именем. Не подключается и экземпляр не занимает. Нужен потому, что
        /// подключение .NET (NamedPipeClientStream.Connect), пока канала нет, крутит ядро процессора вхолостую.
        public static bool Exists(string server, string name)
        {
            if (WaitNamedPipe("\\\\" + server + "\\pipe\\" + name, 1)) return true;
            return Marshal.GetLastWin32Error() != 2;  // 2 — такого канала нет; 121 (время вышло) — есть, но занят
        }

        // ----- кто есть кто -----

        public static SecurityIdentifier Me()
        {
            using (WindowsIdentity identity = WindowsIdentity.GetCurrent()) return identity.User;
        }

        /// Пользователь сеанса Windows, в котором работает процесс: тот, кто сидит за этим рабочим столом.
        public static SecurityIdentifier SessionUser()
        {
            string user = Session(5 /* WTSUserName */), domain = Session(7 /* WTSDomainName */);
            return (SecurityIdentifier)new NTAccount(domain, user).Translate(typeof(SecurityIdentifier));
        }

        static string Session(int what)
        {
            IntPtr buffer;
            int bytes;
            if (!WTSQuerySessionInformation(IntPtr.Zero, -1 /* этот сеанс */, what, out buffer, out bytes)) throw new Win32Exception();
            try { return Marshal.PtrToStringUni(buffer); }
            finally { WTSFreeMemory(buffer); }
        }

        /// От чьего имени работает оболочка рабочего стола (Проводник), с уровнем целостности её процесса.
        public static SecurityIdentifier ShellUser(out string integrity)
        {
            const uint TokenQuery = 0x8;
            integrity = null;
            uint process;
            IntPtr shell = GetShellWindow();
            if (shell == IntPtr.Zero) throw new InvalidOperationException("оболочки рабочего стола нет");
            GetWindowThreadProcessId(shell, out process);
            IntPtr handle = OpenProcess(QueryLimited, false, process);
            if (handle == IntPtr.Zero) throw new Win32Exception();
            try
            {
                IntPtr token;
                if (!OpenProcessToken(handle, TokenQuery, out token)) throw new Win32Exception();
                try
                {
                    integrity = Integrity(token);
                    using (var identity = new WindowsIdentity(token)) return identity.User;
                }
                finally { CloseHandle(token); }
            }
            finally { CloseHandle(handle); }
        }

        /// Уровень целостности по маркеру доступа — см. Medium и High.
        public static string Integrity(IntPtr token)
        {
            int size;
            GetTokenInformation(token, 25 /* TokenIntegrityLevel */, IntPtr.Zero, 0, out size);
            IntPtr buffer = Marshal.AllocHGlobal(size);
            try
            {
                if (!GetTokenInformation(token, 25, buffer, size, out size)) throw new Win32Exception();
                return new SecurityIdentifier(Marshal.ReadIntPtr(buffer)).Value;
            }
            finally { Marshal.FreeHGlobal(buffer); }
        }

        /// Уровень целостности этого процесса.
        public static string Integrity()
        {
            using (WindowsIdentity me = WindowsIdentity.GetCurrent()) return Integrity(me.Token);
        }

        /// Какая половина разделённого маркера у процесса: 1 — маркер не разделён (контроль учётных записей
        /// выключен или обычный пользователь), 2 — половина с правами, 3 — без прав; 0 — узнать не удалось.
        public static int ElevationType()
        {
            int size;
            IntPtr buffer = Marshal.AllocHGlobal(4);
            try
            {
                using (WindowsIdentity me = WindowsIdentity.GetCurrent())
                    return GetTokenInformation(me.Token, 18 /* TokenElevationType */, buffer, 4, out size) ? Marshal.ReadInt32(buffer) : 0;
            }
            finally { Marshal.FreeHGlobal(buffer); }
        }

        const uint QueryLimited = 0x1000;

        [StructLayout(LayoutKind.Sequential)]
        struct SecurityAttributes
        {
            public int Length;
            public IntPtr Descriptor;
            public int Inherit;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern SafePipeHandle CreateNamedPipe(string name, uint openMode, uint pipeMode, uint maxInstances, uint outSize, uint inSize,
                                                     uint defaultTimeout, ref SecurityAttributes attributes);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool WaitNamedPipe(string name, uint milliseconds);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetNamedPipeClientProcessId(SafePipeHandle pipe, out uint process);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetNamedPipeServerProcessId(SafePipeHandle pipe, out uint process);
        [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, uint process);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder name, ref uint size);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool CancelIoEx(SafePipeHandle handle, IntPtr overlapped);
        [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr memory);
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern bool ConvertStringSecurityDescriptorToSecurityDescriptor(string sddl, uint revision, out IntPtr descriptor, out uint size);
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern bool ConvertSecurityDescriptorToStringSecurityDescriptor(IntPtr descriptor, uint revision, uint parts, out IntPtr sddl, out uint length);
        [DllImport("advapi32.dll")]
        static extern uint GetSecurityInfo(SafePipeHandle handle, int type, uint parts, out IntPtr owner, out IntPtr group, out IntPtr dacl,
                                           out IntPtr sacl, out IntPtr descriptor);
        [DllImport("advapi32.dll", SetLastError = true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
        [DllImport("advapi32.dll", SetLastError = true)]
        static extern bool GetTokenInformation(IntPtr token, int what, IntPtr buffer, int size, out int needed);
        [DllImport("wtsapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern bool WTSQuerySessionInformation(IntPtr server, int session, int what, out IntPtr buffer, out int bytes);
        [DllImport("wtsapi32.dll")] static extern void WTSFreeMemory(IntPtr memory);
        [DllImport("user32.dll")] static extern IntPtr GetShellWindow();
        [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    }
}
