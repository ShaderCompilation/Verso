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
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

    const ALT_STACK_SIZE: usize = 32 * 1024;
    static mut ALT_STACK: [u8; ALT_STACK_SIZE] = [0; ALT_STACK_SIZE];

    const FATAL_SIGNALS: &[libc::c_int] = &[libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT];

    /// The thread that crashed first (its `pthread_self`, 0 before that) and
    /// the signal it got, which is what the process dies with.
    static CRASHED_THREAD: AtomicUsize = AtomicUsize::new(0);
    static FIRST_SIGNAL: AtomicI32 = AtomicI32::new(0);
    /// Guards against infinite re-entry if the nested-fault path itself faults.
    static NESTED_FAULT: AtomicBool = AtomicBool::new(false);

    /// Installs an alternate signal stack and `SA_SIGINFO` handlers for fatal
    /// signals so native/FFI crashes write signal, fault address, registers, a
    /// raw backtrace and the module offset to stderr before dying with the
    /// original signal.
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
            // Not SA_RESETHAND, and SA_NODEFER with an empty mask: a fault inside
            // the handler (including the same signal), or in another thread
            // meanwhile, must come back to the handler, which dies with the
            // first signal, not the new one.
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER;
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

    /// Return addresses are looked for in this much of the stack.
    const SCAN_BYTES: usize = 128 * 1024;
    const MAX_FRAMES: usize = 64;
    const MAX_MODULES: usize = 32;
    const MAX_NAME: usize = 40;
    const ET_EXEC: u16 = 2;

    /// Load bias to subtract from runtime addresses in a module mapped at
    /// `mapping_base`: 0 for non-PIE `ET_EXEC` binaries (whose runtime
    /// addresses already match ELF virtual addresses for `addr2line`), and
    /// `mapping_base` for `ET_DYN` (PIE and shared libraries).
    pub(super) fn module_load_bias(mapping_base: usize, elf_header: &[u8]) -> usize {
        if elf_header.len() >= 18
            && &elf_header[..4] == b"\x7fELF"
            && u16::from_ne_bytes([elf_header[16], elf_header[17]]) == ET_EXEC
        {
            0
        } else {
            mapping_base
        }
    }

    /// An executable mapping of a file. `base` is where the file is loaded.
    #[derive(Clone, Copy)]
    struct Module {
        start: usize,
        end: usize,
        base: usize,
        name: [u8; MAX_NAME],
        name_len: usize,
    }

    /// What the backtrace needs from `/proc/self/maps`. Static, as alternate
    /// stacks can be 8 KiB; only the thread that crashed first uses it.
    struct Maps {
        modules: [Module; MAX_MODULES],
        count: usize,
        /// The readable mapping the stack pointer is in.
        stack: (usize, usize),
        /// The file of the last mapping, and where its first one starts.
        file: [u8; MAX_NAME],
        file_len: usize,
        base: usize,
        line: [u8; 256],
        chunk: [u8; 1024],
    }

    static mut MAPS: Maps = Maps {
        modules: [Module { start: 0, end: 0, base: 0, name: [0; MAX_NAME], name_len: 0 }; MAX_MODULES],
        count: 0,
        stack: (0, 0),
        file: [0; MAX_NAME],
        file_len: 0,
        base: 0,
        line: [0; 256],
        chunk: [0; 1024],
    };

    fn parse_hex(bytes: &[u8]) -> usize {
        bytes.iter().map_while(|&b| (b as char).to_digit(16)).fold(0, |v, d| (v << 4) | d as usize)
    }

    impl Maps {
        fn load(&mut self, sp: usize) {
            // SAFETY: `open`, `read` and `close` are async-signal-safe.
            let fd = unsafe { libc::open(c"/proc/self/maps".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                return;
            }
            let mut len = 0;
            loop {
                // SAFETY: reads into our own buffer.
                let n = unsafe { libc::read(fd, self.chunk.as_mut_ptr().cast(), self.chunk.len()) };
                if n <= 0 {
                    break;
                }
                for i in 0..n as usize {
                    let byte = self.chunk[i];
                    if byte == b'\n' {
                        self.add_line(len, sp);
                        len = 0;
                    } else if len < self.line.len() {
                        // The end of an overlong path is dropped.
                        self.line[len] = byte;
                        len += 1;
                    }
                }
            }
            // SAFETY: our own descriptor.
            unsafe { libc::close(fd) };
        }

        /// One line of `/proc/self/maps`: `start-end perms offset dev inode path`.
        fn add_line(&mut self, len: usize, sp: usize) {
            let line = &self.line[..len];
            let mut fields = line.split(|&b| b == b' ');
            let (Some(range), Some(perms)) = (fields.next(), fields.next()) else { return };
            let Some(dash) = range.iter().position(|&b| b == b'-') else { return };
            let (start, end) = (parse_hex(&range[..dash]), parse_hex(&range[dash + 1..]));
            let readable = perms.first() == Some(&b'r');
            // After a stack overflow the stack pointer is in or just below the
            // guard area, so the stack is the mapping above it.
            if self.stack.1 == 0 && readable && sp < end && start.saturating_sub(sp) <= SCAN_BYTES {
                self.stack = (start, end);
            }
            let Some(slash) = line.iter().position(|&b| b == b'/') else { return };
            let path = &line[slash..];
            let name = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
            let name_len = name.len().min(MAX_NAME);
            // Not the file offset: small files can have several mappings at 0.
            if name[..name_len] != self.file[..self.file_len] {
                self.file[..name_len].copy_from_slice(&name[..name_len]);
                self.file_len = name_len;
                self.base = if readable && end.saturating_sub(start) >= 18 {
                    // SAFETY: `start..end` is a readable mapping of at least 18 bytes
                    // reported by `/proc/self/maps`; `[u8; 18]` has alignment 1.
                    let hdr = unsafe { ptr::read_volatile(start as *const [u8; 18]) };
                    module_load_bias(start, &hdr)
                } else {
                    start
                };
            }
            if readable && perms.get(2) == Some(&b'x') && self.count < MAX_MODULES {
                let module = Module { start, end, base: self.base, name: self.file, name_len };
                self.modules[self.count] = module;
                self.count += 1;
            }
        }

        fn module(&self, addr: usize) -> Option<&Module> {
            self.modules[..self.count].iter().find(|m| (m.start..m.end).contains(&addr))
        }
    }

    /// Whether the 8 bytes of ARM code just before a return address end in a call.
    #[cfg(any(test, target_arch = "arm"))]
    pub(super) fn is_arm_call(thumb: bool, before: [u16; 4]) -> bool {
        let [earlier @ .., first, second] = before;
        if thumb {
            // BL and BLX to an address, or BLX to a register.
            (first & 0xf800 == 0xf000 && second & 0xc000 == 0xc000) || second & 0xff87 == 0x4780
        } else {
            // BL, BLX to an address or a register, or the branch after
            // `mov lr, pc` (how LLVM calls functions that never return).
            let word = u32::from(second) << 16 | u32::from(first);
            word & 0x0f00_0000 == 0x0b00_0000
                || word & 0xfe00_0000 == 0xfa00_0000
                || word & 0x0fff_fff0 == 0x012f_ff30
                || (earlier == [0xe00f, 0xe1a0] && word & 0x0f00_0000 == 0x0a00_0000)
        }
    }

    /// The module of a value that can be a return address.
    fn return_address(maps: &Maps, value: usize) -> Option<&Module> {
        #[cfg(target_arch = "arm")]
        {
            let thumb = value & 1 == 1;
            let addr = value & !1;
            let module = maps.module(addr)?;
            if addr < module.start + 8 || (!thumb && addr & 3 != 0) {
                return None;
            }
            // SAFETY: within a readable mapping, and 2-byte aligned.
            let before = unsafe { ptr::read_volatile((addr - 8) as *const [u16; 4]) };
            is_arm_call(thumb, before).then_some(module)
        }
        #[cfg(not(target_arch = "arm"))]
        maps.module(value)
    }

    fn push_frame(out: &mut StackBuf, number: usize, addr: usize, module: Option<&Module>) {
        out.push_str("  #");
        out.push_u64(number as u64);
        out.push_str(" ");
        match module {
            Some(m) => {
                out.push_bytes(&m.name[..m.name_len]);
                out.push_str("+");
                out.push_hex(addr.saturating_sub(m.base));
            }
            None => out.push_hex(addr),
        }
        out.push_str("\n");
        out.flush_stderr();
    }

    /// Writes pc, lr and every value on the stack that follows a call, newest
    /// first, as `module+offset`. Stale addresses left in unused stack slots
    /// can be among them.
    ///
    /// Not glibc's `backtrace()`: it loads the Kindle's libgcc_s, whose ARM
    /// unwinder hands its own context to the personality routines linked into
    /// Verso, which pass it to LLVM's libunwind (Zig links that). The two
    /// contexts differ, so it crashed on the first frame with a personality
    /// routine. Unwinding also takes the dynamic linker's lock. Reading
    /// `/proc/self/maps` and the stack needs neither an unwinder nor a lock.
    fn write_backtrace(out: &mut StackBuf, regs: &Regs) {
        // SAFETY: only the thread that crashed first gets here, once.
        let maps = unsafe { &mut *ptr::addr_of_mut!(MAPS) };
        maps.load(regs.sp);
        let maps = &*maps;
        out.push_str("fatal backtrace (pc, lr, then calls found on the stack; some may be stale):\n");
        push_frame(out, 0, regs.pc, maps.module(regs.pc));
        let mut frames = 1;
        let mut last = regs.lr.unwrap_or(0);
        if let Some(module) = return_address(maps, last) {
            push_frame(out, frames, last & !usize::from(cfg!(target_arch = "arm")), Some(module));
            frames += 1;
        }
        let word = size_of::<usize>();
        let (low, high) = maps.stack;
        let Some(mut at) = regs.sp.max(low).checked_next_multiple_of(word) else { return };
        let end = high.min(at.saturating_add(SCAN_BYTES));
        while end.saturating_sub(at) >= word && frames < MAX_FRAMES {
            // SAFETY: within a readable mapping, and aligned.
            let value = unsafe { ptr::read_volatile(at as *const usize) };
            if value != last
                && let Some(module) = return_address(maps, value)
            {
                push_frame(out, frames, value & !usize::from(cfg!(target_arch = "arm")), Some(module));
                frames += 1;
                last = value;
            }
            at += word;
        }
    }

    /// Ends the process with `sig`, whatever else is going on.
    fn die(sig: libc::c_int) -> ! {
        // SAFETY: all async-signal-safe, on our own data.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = libc::SIG_DFL;
            libc::sigaction(sig, &sa, ptr::null_mut());
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, sig);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, ptr::null_mut());
            libc::raise(sig);
            libc::_exit(128 + sig);
        }
    }

    pub(super) extern "C" fn handler(sig: libc::c_int, info: *mut libc::siginfo_t, uctx: *mut libc::c_void) {
        // SAFETY: only reads the thread's own handle.
        let thread = unsafe { libc::pthread_self() } as usize;
        match CRASHED_THREAD.compare_exchange(0, thread, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => FIRST_SIGNAL.store(sig, Ordering::Release),
            Err(first) if first == thread => {
                let first = FIRST_SIGNAL.load(Ordering::Acquire);
                let first_sig = if first != 0 { first } else { sig };
                if NESTED_FAULT.swap(true, Ordering::AcqRel) {
                    // SAFETY: `_exit` is async-signal-safe and avoids infinite
                    // re-entry if the nested-fault path itself faults.
                    unsafe { libc::_exit(128 + first_sig) };
                }
                let note = b"fatal signal handler faulted\n";
                // SAFETY: `write` to fd 2 is async-signal-safe.
                unsafe { libc::write(libc::STDERR_FILENO, note.as_ptr().cast(), note.len()) };
                die(first_sig);
            }
            Err(_) => {
                // Another thread is writing its crash: let it finish.
                // SAFETY: `sleep` is async-signal-safe.
                unsafe { libc::sleep(2) };
                let first = FIRST_SIGNAL.load(Ordering::Acquire);
                die(if first != 0 { first } else { sig });
            }
        }

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

        // 2. Raw backtrace: no locks, no heap.
        if let Some(r) = &regs {
            write_backtrace(&mut out, r);
        }

        // 3. Last, as it takes the dynamic linker's lock: the symbol, via dladdr.
        // Bound with alarm(3) so a deadlock on `_dl_load_lock` cannot hang exit.
        if let Some(r) = &regs
            && r.pc != 0
        {
            // SAFETY: `Dl_info` is a POD struct; `dladdr` does not allocate;
            // `alarm` is async-signal-safe.
            unsafe {
                let mut dl: libc::Dl_info = std::mem::zeroed();
                libc::alarm(3);
                let found = libc::dladdr(r.pc as *const libc::c_void, &mut dl) != 0;
                libc::alarm(0);
                if found {
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

        // 4. Die with the original signal.
        die(sig);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;

    extern "C" fn nested_segv_wrapper(sig: libc::c_int, _info: *mut libc::siginfo_t, _uctx: *mut libc::c_void) {
        // Unmapped siginfo triggers a synchronous SIGSEGV while inside the
        // kernel-delivered SIGSEGV handler.
        super::crash::handler(sig, std::ptr::without_provenance_mut(0x1000), std::ptr::null_mut());
    }

    #[test]
    fn fatal_signal_handler_logs_and_dies_with_the_first_signal() {
        match std::env::var("VERSO_TEST_CRASH").as_deref() {
            Ok("segv") => {
                super::crash::install();
                // SAFETY: deliberate signal in isolated test child process.
                unsafe { libc::raise(libc::SIGSEGV) };
                std::process::exit(0);
            }
            Ok("nested") => {
                super::crash::install();
                // The unmapped siginfo makes the handler itself fault.
                super::crash::handler(libc::SIGABRT, std::ptr::without_provenance_mut(0x1000), std::ptr::null_mut());
                std::process::exit(0);
            }
            Ok("nested_same") => {
                super::crash::install();
                // Keep the exact sa_flags and sa_mask installed by `crash::install()`,
                // swapping only the entry point so the handler faults with SIGSEGV
                // while handling a kernel-delivered SIGSEGV (requires SA_NODEFER).
                // SAFETY: isolated test child process.
                unsafe {
                    let mut sa: libc::sigaction = std::mem::zeroed();
                    assert_eq!(libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut sa), 0);
                    sa.sa_sigaction = nested_segv_wrapper as *const () as usize;
                    assert_eq!(libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut()), 0);
                    libc::raise(libc::SIGSEGV);
                }
                std::process::exit(0);
            }
            _ => {}
        }
        let crash = |how: &str| {
            let name = "logging::tests::fatal_signal_handler_logs_and_dies_with_the_first_signal";
            let out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env("VERSO_TEST_CRASH", how)
                .output()
                .unwrap();
            (out.status.signal(), String::from_utf8_lossy(&out.stderr).into_owned())
        };

        let (signal, stderr) = crash("segv");
        assert_eq!(signal, Some(libc::SIGSEGV), "child stderr:\n{stderr}");
        assert!(stderr.contains("fatal signal: SIGSEGV (11)"), "missing signal line:\n{stderr}");
        assert!(stderr.contains("pc=0x"), "missing pc register:\n{stderr}");
        #[cfg(not(all(target_env = "musl", target_feature = "crt-static")))]
        assert!(stderr.contains("fatal location:"), "missing dladdr location:\n{stderr}");
        let backtrace = stderr.split("fatal backtrace").nth(1).unwrap_or_default();
        assert!(backtrace.contains("  #1 ") && backtrace.contains(" verso-"), "no backtrace:\n{stderr}");

        // A fault inside the handler must not replace the first signal.
        let (signal, stderr) = crash("nested");
        assert_eq!(signal, Some(libc::SIGABRT), "child stderr:\n{stderr}");
        assert!(stderr.contains("fatal signal handler faulted"), "child stderr:\n{stderr}");

        // A nested same-signal fault (SIGSEGV inside SIGSEGV) must re-enter the handler.
        let (signal, stderr) = crash("nested_same");
        assert_eq!(signal, Some(libc::SIGSEGV), "child stderr:\n{stderr}");
        assert!(stderr.contains("fatal signal handler faulted"), "child stderr:\n{stderr}");
    }

    #[test]
    fn arm_calls_are_told_from_other_code() {
        let after = |before: u32, word: u32| {
            super::crash::is_arm_call(false, [before as u16, (before >> 16) as u16, word as u16, (word >> 16) as u16])
        };
        let arm = |word| after(0xe5901000, word);
        assert!(after(0xe1a0e00f, 0xea0000c1), "mov lr, pc, then b");
        assert!(arm(0xeb0000c1) && arm(0x0b0000c1), "bl, bleq");
        assert!(arm(0xfa0000c1) && arm(0xe12fff33), "blx to an address, blx r3");
        assert!(!arm(0xea0000c1) && !arm(0xe12fff13) && !arm(0xe5901000), "b, bx r3, ldr");
        let thumb = |first, second| super::crash::is_arm_call(true, [0, 0, first, second]);
        assert!(thumb(0xf7ff, 0xfffe) && thumb(0xf000, 0xe800) && thumb(0x0000, 0x4798), "bl, blx, blx r3");
        assert!(!thumb(0xf000, 0xb800) && !thumb(0x0000, 0x4718), "b.w, bx r3");
    }

    #[test]
    fn module_load_bias_handles_et_exec_and_et_dyn() {
        let mut hdr = [0u8; 18];
        hdr[..4].copy_from_slice(b"\x7fELF");
        hdr[16..18].copy_from_slice(&2u16.to_ne_bytes()); // ET_EXEC
        assert_eq!(super::crash::module_load_bias(0x10000, &hdr), 0);

        hdr[16..18].copy_from_slice(&3u16.to_ne_bytes()); // ET_DYN
        assert_eq!(super::crash::module_load_bias(0x7f12_0000, &hdr), 0x7f12_0000);

        assert_eq!(super::crash::module_load_bias(0x20000, b"not-elf-at-all-bytes"), 0x20000);
        assert_eq!(super::crash::module_load_bias(0x20000, &hdr[..17]), 0x20000);
    }
}
