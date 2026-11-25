//! Logging and crash reporting setup.
//!
//! The app logs to stderr; on the Kindle the launcher script sends stderr to
//! `/mnt/us/verso/verso.log`, so panics and fatal-signal diagnostics land there
//! too. Set `RUST_LOG` (e.g. `RUST_LOG=debug`) to see more.

pub fn init() {
    builder().init();
    panic_hook();
    #[cfg(unix)]
    crash::install();
}

/// For a helper process sharing the app's log: each line names it.
pub fn init_tagged(tag: &'static str) {
    builder()
        .format(move |buf, record| {
            use std::io::Write;
            writeln!(
                buf,
                "[{} {:<5} {} {tag}] {}",
                buf.timestamp_millis(),
                record.level(),
                record.target(),
                record.args()
            )
        })
        .init();
    panic_hook();
    #[cfg(unix)]
    crash::install();
}

fn builder() -> env_logger::Builder {
    let default = if cfg!(debug_assertions) { "info,verso=debug" } else { "info" };
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default));
    builder.format_timestamp_millis();
    builder
}

fn panic_hook() {
    // Panics abort in release builds; make sure the reason reaches the log first.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        default_hook(info);
    }));
}

#[cfg(unix)]
mod crash {
    use std::ptr;

    const ALT_STACK_SIZE: usize = 32 * 1024;
    static mut ALT_STACK: [u8; ALT_STACK_SIZE] = [0; ALT_STACK_SIZE];

    const FATAL_SIGNALS: &[libc::c_int] = &[libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT];

    /// Installs an alternate signal stack and one-shot `SA_SIGINFO` handlers
    /// for fatal signals so native/FFI crashes write signal, fault address,
    /// registers, module offset and a raw backtrace to stderr before dying with
    /// the original signal.
    pub fn install() {
        // SAFETY: static buffer outlives the process; `sigaltstack` and
        // `sigaction` are standard POSIX calls with POD structs.
        unsafe {
            let ss = libc::stack_t {
                ss_sp: ptr::addr_of_mut!(ALT_STACK).cast::<libc::c_void>(),
                ss_flags: 0,
                ss_size: ALT_STACK_SIZE,
            };
            let _ = libc::sigaltstack(&ss, ptr::null_mut());

            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler as *const () as usize;
            // SA_RESETHAND ensures re-raising (or a nested fault inside the
            // handler) terminates immediately with the default signal action.
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
            libc::sigemptyset(&mut sa.sa_mask);
            for &sig in FATAL_SIGNALS {
                let _ = libc::sigaction(sig, &sa, ptr::null_mut());
            }
        }
    }

    struct StackBuf {
        buf: [u8; 512],
        len: usize,
    }

    impl StackBuf {
        const fn new() -> Self {
            Self { buf: [0; 512], len: 0 }
        }

        fn push_bytes(&mut self, bytes: &[u8]) {
            let avail = self.buf.len().saturating_sub(self.len);
            let n = bytes.len().min(avail);
            self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
            self.len += n;
        }

        fn push_str(&mut self, s: &str) {
            self.push_bytes(s.as_bytes());
        }

        fn push_cstr(&mut self, p: *const libc::c_char, max_len: usize) {
            if p.is_null() {
                return;
            }
            let mut n = 0usize;
            while n < max_len {
                // SAFETY: caller passes a valid NUL-terminated C string from `dladdr`.
                let b = unsafe { *p.add(n) as u8 };
                if b == 0 {
                    break;
                }
                self.push_bytes(&[b]);
                n += 1;
            }
        }

        fn push_i32(&mut self, v: i32) {
            if v < 0 {
                self.push_str("-");
                self.push_u64(v.unsigned_abs() as u64);
            } else {
                self.push_u64(v as u64);
            }
        }

        fn push_u64(&mut self, mut v: u64) {
            let mut tmp = [0u8; 20];
            let mut i = tmp.len();
            if v == 0 {
                self.push_bytes(b"0");
                return;
            }
            while v > 0 {
                i -= 1;
                tmp[i] = b'0' + (v % 10) as u8;
                v /= 10;
            }
            self.push_bytes(&tmp[i..]);
        }

        fn push_hex(&mut self, mut v: usize) {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            self.push_bytes(b"0x");
            let mut tmp = [0u8; 16];
            let mut i = tmp.len();
            if v == 0 {
                self.push_bytes(b"0");
                return;
            }
            while v > 0 {
                i -= 1;
                tmp[i] = HEX[v & 0xf];
                v >>= 4;
            }
            self.push_bytes(&tmp[i..]);
        }

        fn flush_stderr(&mut self) {
            if self.len > 0 {
                // SAFETY: `write` to fd 2 is async-signal-safe.
                unsafe {
                    let _ = libc::write(libc::STDERR_FILENO, self.buf.as_ptr().cast(), self.len);
                }
                self.len = 0;
            }
        }
    }

    fn signal_name(sig: libc::c_int) -> &'static str {
        match sig {
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGBUS => "SIGBUS",
            libc::SIGILL => "SIGILL",
            libc::SIGFPE => "SIGFPE",
            libc::SIGABRT => "SIGABRT",
            _ => "SIGNAL",
        }
    }

    fn si_code_name(sig: libc::c_int, code: libc::c_int) -> Option<&'static str> {
        #[cfg(target_os = "linux")]
        {
            // Linux `<asm-generic/siginfo.h>` ABI values (stable across ARM & x86).
            if code == libc::SI_USER {
                return Some("SI_USER");
            }
            if code == libc::SI_TKILL {
                return Some("SI_TKILL");
            }
            match (sig, code) {
                (libc::SIGSEGV, 1) => Some("SEGV_MAPERR"),
                (libc::SIGSEGV, 2) => Some("SEGV_ACCERR"),
                (libc::SIGBUS, 1) => Some("BUS_ADRALN"),
                (libc::SIGBUS, 2) => Some("BUS_ADRERR"),
                (libc::SIGBUS, 3) => Some("BUS_OBJERR"),
                (libc::SIGILL, 1) => Some("ILL_ILLOPC"),
                (libc::SIGILL, 2) => Some("ILL_ILLOPN"),
                (libc::SIGILL, 3) => Some("ILL_ILLADR"),
                (libc::SIGILL, 5) => Some("ILL_PRVOPC"),
                (libc::SIGFPE, 1) => Some("FPE_INTDIV"),
                (libc::SIGFPE, 3) => Some("FPE_FLTDIV"),
                _ => None,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (sig, code);
            None
        }
    }

    struct Regs {
        pc: usize,
        sp: usize,
        lr: Option<usize>,
    }

    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    unsafe fn extract_regs(uctx: *mut libc::c_void) -> Option<Regs> {
        if uctx.is_null() {
            return None;
        }
        let uc = unsafe { &*(uctx as *const libc::ucontext_t) };
        Some(Regs {
            pc: uc.uc_mcontext.arm_pc as usize,
            sp: uc.uc_mcontext.arm_sp as usize,
            lr: Some(uc.uc_mcontext.arm_lr as usize),
        })
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    unsafe fn extract_regs(uctx: *mut libc::c_void) -> Option<Regs> {
        if uctx.is_null() {
            return None;
        }
        let uc = unsafe { &*(uctx as *const libc::ucontext_t) };
        Some(Regs {
            pc: uc.uc_mcontext.gregs[libc::REG_RIP as usize] as usize,
            sp: uc.uc_mcontext.gregs[libc::REG_RSP as usize] as usize,
            lr: None,
        })
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    unsafe fn extract_regs(uctx: *mut libc::c_void) -> Option<Regs> {
        if uctx.is_null() {
            return None;
        }
        let uc = unsafe { &*(uctx as *const libc::ucontext_t) };
        Some(Regs { pc: uc.uc_mcontext.pc as usize, sp: uc.uc_mcontext.sp as usize, lr: None })
    }

    #[cfg(not(all(target_os = "linux", any(target_arch = "arm", target_arch = "x86_64", target_arch = "aarch64"))))]
    unsafe fn extract_regs(_uctx: *mut libc::c_void) -> Option<Regs> {
        None
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe extern "C" {
        fn backtrace(buffer: *mut *mut libc::c_void, size: libc::c_int) -> libc::c_int;
        fn backtrace_symbols_fd(buffer: *const *mut libc::c_void, size: libc::c_int, fd: libc::c_int);
    }

    extern "C" fn handler(sig: libc::c_int, info: *mut libc::siginfo_t, uctx: *mut libc::c_void) {
        // 1. Strictly async-signal-safe primary line: always flushed first.
        let mut out = StackBuf::new();
        out.push_str("fatal signal: ");
        out.push_str(signal_name(sig));
        out.push_str(" (");
        out.push_i32(sig);
        out.push_str(")");
        if !info.is_null() {
            // SAFETY: kernel supplies a valid siginfo_t for SA_SIGINFO handlers.
            let (code, addr) = unsafe { ((*info).si_code, (*info).si_addr() as usize) };
            out.push_str(" code=");
            out.push_i32(code);
            if let Some(name) = si_code_name(sig, code) {
                out.push_str(" (");
                out.push_str(name);
                out.push_str(")");
            }
            out.push_str(" addr=");
            out.push_hex(addr);
        }
        // SAFETY: kernel supplies a valid ucontext_t for SA_SIGINFO handlers.
        let regs = unsafe { extract_regs(uctx) };
        if let Some(r) = &regs {
            out.push_str(" pc=");
            out.push_hex(r.pc);
            if let Some(lr) = r.lr {
                out.push_str(" lr=");
                out.push_hex(lr);
            }
            out.push_str(" sp=");
            out.push_hex(r.sp);
        }
        out.push_str("\n");
        out.flush_stderr();

        // 2. Best-effort module + file offset via dladdr (no heap allocation).
        if let Some(r) = &regs
            && r.pc != 0
        {
            // SAFETY: `Dl_info` is a POD struct; `dladdr` does not allocate.
            unsafe {
                let mut dl: libc::Dl_info = std::mem::zeroed();
                if libc::dladdr(r.pc as *const libc::c_void, &mut dl) != 0 {
                    out.push_str("fatal location: ");
                    if !dl.dli_fname.is_null() {
                        out.push_cstr(dl.dli_fname, 160);
                    } else {
                        out.push_str("<unknown>");
                    }
                    let fbase = dl.dli_fbase as usize;
                    if r.pc >= fbase {
                        out.push_str("+");
                        out.push_hex(r.pc - fbase);
                    }
                    if !dl.dli_sname.is_null() {
                        out.push_str(" (");
                        out.push_cstr(dl.dli_sname, 128);
                        let saddr = dl.dli_saddr as usize;
                        if r.pc >= saddr {
                            out.push_str("+");
                            out.push_hex(r.pc - saddr);
                        }
                        out.push_str(")");
                    }
                    out.push_str("\n");
                    out.flush_stderr();
                }
            }
        }

        // 3. Best-effort raw backtrace directly to fd 2 (glibc uses no malloc).
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        unsafe {
            let mut frames: [*mut libc::c_void; 32] = [ptr::null_mut(); 32];
            let n = backtrace(frames.as_mut_ptr(), frames.len() as libc::c_int);
            if n > 0 {
                out.push_str("fatal backtrace:\n");
                out.flush_stderr();
                backtrace_symbols_fd(frames.as_ptr(), n, libc::STDERR_FILENO);
            }
        }

        // 4. Re-raise with the signal unblocked; SA_RESETHAND already restored SIG_DFL.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, sig);
            libc::sigprocmask(libc::SIG_UNBLOCK, &set, ptr::null_mut());
            libc::raise(sig);
            libc::_exit(128 + sig);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;

    #[test]
    fn fatal_signal_handler_logs_signal_location_and_reraises() {
        if std::env::var("VERSO_TEST_RAISE_SEGV").as_deref() == Ok("1") {
            super::crash::install();
            // SAFETY: deliberate signal in isolated test child process.
            unsafe {
                libc::raise(libc::SIGSEGV);
            }
            std::process::exit(0);
        }

        let exe = std::env::current_exe().unwrap();
        let out = Command::new(exe)
            .args(["--exact", "logging::tests::fatal_signal_handler_logs_signal_location_and_reraises", "--nocapture"])
            .env("VERSO_TEST_RAISE_SEGV", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.signal(), Some(libc::SIGSEGV), "child stderr:\n{stderr}");
        assert!(stderr.contains("fatal signal: SIGSEGV (11)"), "missing signal line:\n{stderr}");
        assert!(stderr.contains("pc=0x"), "missing pc register:\n{stderr}");
        assert!(stderr.contains("fatal location:"), "missing dladdr location:\n{stderr}");
    }
}
