use std::collections::BTreeMap;
use std::fmt;

// windows job object limit flags from winnt.h
const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
const JOB_OBJECT_LIMIT_JOB_MEMORY: u32 = 0x0000_0200;
const JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION: u32 = 0x0000_0400;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

// windows job object ui restriction flags from winnt.h
const JOB_OBJECT_UILIMIT_HANDLES: u32 = 0x0000_0001;
const JOB_OBJECT_UILIMIT_READCLIPBOARD: u32 = 0x0000_0002;
const JOB_OBJECT_UILIMIT_WRITECLIPBOARD: u32 = 0x0000_0004;
const JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS: u32 = 0x0000_0008;
const JOB_OBJECT_UILIMIT_DISPLAYSETTINGS: u32 = 0x0000_0010;
const JOB_OBJECT_UILIMIT_GLOBALATOMS: u32 = 0x0000_0020;
const JOB_OBJECT_UILIMIT_DESKTOP: u32 = 0x0000_0040;
const JOB_OBJECT_UILIMIT_EXITWINDOWS: u32 = 0x0000_0080;

// windows process mitigation policy bitmasks from winnt.h
const MITIGATION_PROHIBIT_DYNAMIC_CODE: u32 = 1 << 0;
const MITIGATION_STRICT_HANDLE_RAISE: u32 = 1 << 0;
const MITIGATION_STRICT_HANDLE_PERMANENT: u32 = 1 << 1;
const MITIGATION_DISABLE_EXTENSION_POINTS: u32 = 1 << 0;
const MITIGATION_NO_REMOTE_IMAGES: u32 = 1 << 0;
const MITIGATION_NO_LOW_LABEL_IMAGES: u32 = 1 << 1;
const MITIGATION_PREFER_SYSTEM32_IMAGES: u32 = 1 << 2;
const MITIGATION_NO_CHILD_PROCESS_CREATION: u32 = 1 << 0;

// windows restricted token flags from winbase.h
const TOKEN_FLAG_DISABLE_MAX_PRIVILEGE: u32 = 0x0000_0001;
const TOKEN_FLAG_SANDBOX_INERT: u32 = 0x0000_0002;
const TOKEN_FLAG_LUA_TOKEN: u32 = 0x0000_0004;
const TOKEN_FLAG_WRITE_RESTRICTED: u32 = 0x0000_0008;

// linux classic bpf and seccomp abi constants from linux/filter.h, linux/seccomp.h, linux/audit.h
const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;
const BPF_MAXINSNS: usize = 4096;

const SECCOMP_DATA_NR_OFFSET: u32 = 0;
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;

const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
const AUDIT_ARCH_AARCH64: u32 = 0xC000_00B7;

const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_KILL_THREAD: u32 = 0x0000_0000;
const SECCOMP_RET_TRAP: u32 = 0x0003_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7FFF_0000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxError {
    operation: &'static str,
    detail: String,
}

impl SandboxError {
    fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }

    pub fn operation(&self) -> &'static str {
        self.operation
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sandbox {} failed: {}", self.operation, self.detail)
    }
}

impl std::error::Error for SandboxError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiRestrictions {
    pub handles: bool,
    pub read_clipboard: bool,
    pub write_clipboard: bool,
    pub system_parameters: bool,
    pub display_settings: bool,
    pub global_atoms: bool,
    pub desktop: bool,
    pub exit_windows: bool,
}

impl UiRestrictions {
    pub const fn renderer_default() -> Self {
        Self {
            handles: true,
            read_clipboard: true,
            write_clipboard: true,
            system_parameters: true,
            display_settings: true,
            global_atoms: true,
            desktop: true,
            exit_windows: true,
        }
    }

    pub const fn raw_bits(self) -> u32 {
        let mut bits = 0;
        if self.handles {
            bits |= JOB_OBJECT_UILIMIT_HANDLES;
        }
        if self.read_clipboard {
            bits |= JOB_OBJECT_UILIMIT_READCLIPBOARD;
        }
        if self.write_clipboard {
            bits |= JOB_OBJECT_UILIMIT_WRITECLIPBOARD;
        }
        if self.system_parameters {
            bits |= JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS;
        }
        if self.display_settings {
            bits |= JOB_OBJECT_UILIMIT_DISPLAYSETTINGS;
        }
        if self.global_atoms {
            bits |= JOB_OBJECT_UILIMIT_GLOBALATOMS;
        }
        if self.desktop {
            bits |= JOB_OBJECT_UILIMIT_DESKTOP;
        }
        if self.exit_windows {
            bits |= JOB_OBJECT_UILIMIT_EXITWINDOWS;
        }
        bits
    }

    pub const fn from_raw_bits(bits: u32) -> Self {
        Self {
            handles: bits & JOB_OBJECT_UILIMIT_HANDLES != 0,
            read_clipboard: bits & JOB_OBJECT_UILIMIT_READCLIPBOARD != 0,
            write_clipboard: bits & JOB_OBJECT_UILIMIT_WRITECLIPBOARD != 0,
            system_parameters: bits & JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS != 0,
            display_settings: bits & JOB_OBJECT_UILIMIT_DISPLAYSETTINGS != 0,
            global_atoms: bits & JOB_OBJECT_UILIMIT_GLOBALATOMS != 0,
            desktop: bits & JOB_OBJECT_UILIMIT_DESKTOP != 0,
            exit_windows: bits & JOB_OBJECT_UILIMIT_EXITWINDOWS != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobLimits {
    pub active_process_limit: Option<u32>,
    pub process_memory_limit: Option<usize>,
    pub job_memory_limit: Option<usize>,
    pub kill_on_close: bool,
    pub die_on_unhandled_exception: bool,
    pub ui_restrictions: UiRestrictions,
}

impl JobLimits {
    pub const fn renderer_default() -> Self {
        Self {
            active_process_limit: Some(1),
            process_memory_limit: None,
            job_memory_limit: None,
            kill_on_close: true,
            die_on_unhandled_exception: true,
            ui_restrictions: UiRestrictions::renderer_default(),
        }
    }

    pub const fn basic_limit_flags(&self) -> u32 {
        let mut flags = 0;
        if self.active_process_limit.is_some() {
            flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        }
        if self.process_memory_limit.is_some() {
            flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        }
        if self.job_memory_limit.is_some() {
            flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        }
        if self.kill_on_close {
            flags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        if self.die_on_unhandled_exception {
            flags |= JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
        }
        flags
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MitigationPolicy {
    pub prohibit_dynamic_code: bool,
    pub strict_handle_checks: bool,
    pub disable_extension_points: bool,
    pub no_remote_images: bool,
    pub no_low_label_images: bool,
    pub prefer_system32_images: bool,
    pub no_child_process_creation: bool,
}

impl MitigationPolicy {
    pub const fn renderer_default(allow_jit: bool) -> Self {
        Self {
            prohibit_dynamic_code: !allow_jit,
            strict_handle_checks: true,
            disable_extension_points: true,
            no_remote_images: true,
            no_low_label_images: true,
            prefer_system32_images: true,
            no_child_process_creation: true,
        }
    }

    pub const fn dynamic_code_flags(&self) -> u32 {
        if self.prohibit_dynamic_code {
            MITIGATION_PROHIBIT_DYNAMIC_CODE
        } else {
            0
        }
    }

    pub const fn strict_handle_flags(&self) -> u32 {
        if self.strict_handle_checks {
            MITIGATION_STRICT_HANDLE_RAISE | MITIGATION_STRICT_HANDLE_PERMANENT
        } else {
            0
        }
    }

    pub const fn extension_point_flags(&self) -> u32 {
        if self.disable_extension_points {
            MITIGATION_DISABLE_EXTENSION_POINTS
        } else {
            0
        }
    }

    pub const fn image_load_flags(&self) -> u32 {
        let mut flags = 0;
        if self.no_remote_images {
            flags |= MITIGATION_NO_REMOTE_IMAGES;
        }
        if self.no_low_label_images {
            flags |= MITIGATION_NO_LOW_LABEL_IMAGES;
        }
        if self.prefer_system32_images {
            flags |= MITIGATION_PREFER_SYSTEM32_IMAGES;
        }
        flags
    }

    pub const fn child_process_flags(&self) -> u32 {
        if self.no_child_process_creation {
            MITIGATION_NO_CHILD_PROCESS_CREATION
        } else {
            0
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MitigationStatus {
    pub prohibit_dynamic_code: bool,
    pub strict_handle_checks: bool,
    pub disable_extension_points: bool,
    pub no_remote_images: bool,
    pub no_low_label_images: bool,
    pub prefer_system32_images: bool,
    pub no_child_process_creation: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestrictedTokenConfig {
    pub disable_max_privilege: bool,
    pub sandbox_inert: bool,
    pub lua_token: bool,
    pub write_restricted: bool,
    pub restrict_to_restricted_code_sid: bool,
}

impl RestrictedTokenConfig {
    pub const fn renderer_default() -> Self {
        Self {
            disable_max_privilege: true,
            sandbox_inert: false,
            lua_token: true,
            write_restricted: false,
            restrict_to_restricted_code_sid: true,
        }
    }

    pub const fn raw_flags(&self) -> u32 {
        let mut flags = 0;
        if self.disable_max_privilege {
            flags |= TOKEN_FLAG_DISABLE_MAX_PRIVILEGE;
        }
        if self.sandbox_inert {
            flags |= TOKEN_FLAG_SANDBOX_INERT;
        }
        if self.lua_token {
            flags |= TOKEN_FLAG_LUA_TOKEN;
        }
        if self.write_restricted {
            flags |= TOKEN_FLAG_WRITE_RESTRICTED;
        }
        flags
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceLimits {
    pub core_bytes: Option<u64>,
    pub max_open_files: Option<u64>,
    pub address_space_bytes: Option<u64>,
    pub max_processes: Option<u64>,
}

impl ResourceLimits {
    pub const fn renderer_default() -> Self {
        Self {
            core_bytes: Some(0),
            max_open_files: None,
            address_space_bytes: None,
            max_processes: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeccompArch {
    X86_64,
    Aarch64,
}

impl SeccompArch {
    pub const fn current() -> Option<Self> {
        #[cfg(target_arch = "x86_64")]
        {
            Some(Self::X86_64)
        }
        #[cfg(target_arch = "aarch64")]
        {
            Some(Self::Aarch64)
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            None
        }
    }

    pub const fn audit_arch(self) -> u32 {
        match self {
            Self::X86_64 => AUDIT_ARCH_X86_64,
            Self::Aarch64 => AUDIT_ARCH_AARCH64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeccompAction {
    KillProcess,
    KillThread,
    Trap,
    Errno(u16),
    Allow,
}

impl SeccompAction {
    pub const fn raw(self) -> u32 {
        match self {
            Self::KillProcess => SECCOMP_RET_KILL_PROCESS,
            Self::KillThread => SECCOMP_RET_KILL_THREAD,
            Self::Trap => SECCOMP_RET_TRAP,
            Self::Errno(code) => SECCOMP_RET_ERRNO | (code as u32),
            Self::Allow => SECCOMP_RET_ALLOW,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SockFprog {
    pub len: u16,
    pub filter: *const SockFilter,
}

const _: () = assert!(size_of::<SockFilter>() == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<SockFprog>() == 16);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeccompFilter {
    arch: SeccompArch,
    default_action: SeccompAction,
    rules: BTreeMap<u32, SeccompAction>,
}

impl SeccompFilter {
    pub fn new(arch: SeccompArch, default_action: SeccompAction) -> Self {
        Self {
            arch,
            default_action,
            rules: BTreeMap::new(),
        }
    }

    pub fn for_current_arch(default_action: SeccompAction) -> Result<Self, SandboxError> {
        let arch = SeccompArch::current().ok_or_else(|| {
            SandboxError::new(
                "seccomp_arch",
                "unsupported target architecture for seccomp",
            )
        })?;
        Ok(Self::new(arch, default_action))
    }

    pub fn arch(&self) -> SeccompArch {
        self.arch
    }

    pub fn default_action(&self) -> SeccompAction {
        self.default_action
    }

    pub fn allow(mut self, syscall: u32) -> Self {
        self.rules.insert(syscall, SeccompAction::Allow);
        self
    }

    pub fn allow_many(mut self, syscalls: &[u32]) -> Self {
        for &syscall in syscalls {
            self.rules.insert(syscall, SeccompAction::Allow);
        }
        self
    }

    pub fn rule(mut self, syscall: u32, action: SeccompAction) -> Self {
        self.rules.insert(syscall, action);
        self
    }

    pub fn compile(&self) -> Result<Vec<SockFilter>, SandboxError> {
        // group rules by target action so each chunk shares a single return instruction while
        // keeping every bpf relative jump offset strictly within u8 bounds
        let mut by_action: BTreeMap<SeccompAction, Vec<u32>> = BTreeMap::new();
        for (&syscall, &action) in &self.rules {
            if action != self.default_action {
                by_action.entry(action).or_default().push(syscall);
            }
        }

        let mut program = Vec::with_capacity(self.rules.len() + 8);
        // validate seccomp_data.arch before inspecting syscall numbers
        program.push(SockFilter {
            code: BPF_LD | BPF_W | BPF_ABS,
            jt: 0,
            jf: 0,
            k: SECCOMP_DATA_ARCH_OFFSET,
        });
        program.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 1,
            jf: 0,
            k: self.arch.audit_arch(),
        });
        program.push(SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_KILL_PROCESS,
        });
        program.push(SockFilter {
            code: BPF_LD | BPF_W | BPF_ABS,
            jt: 0,
            jf: 0,
            k: SECCOMP_DATA_NR_OFFSET,
        });

        for (action, syscalls) in by_action {
            for chunk in syscalls.chunks(254) {
                let len = chunk.len();
                for (index, &nr) in chunk.iter().enumerate() {
                    let is_last = index + 1 == len;
                    program.push(SockFilter {
                        code: BPF_JMP | BPF_JEQ | BPF_K,
                        jt: (len - 1 - index) as u8,
                        jf: u8::from(is_last),
                        k: nr,
                    });
                }
                program.push(SockFilter {
                    code: BPF_RET | BPF_K,
                    jt: 0,
                    jf: 0,
                    k: action.raw(),
                });
            }
        }

        program.push(SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: self.default_action.raw(),
        });

        if program.len() > BPF_MAXINSNS {
            return Err(SandboxError::new(
                "seccomp_compile",
                format!(
                    "filter program has {} instructions, exceeding BPF_MAXINSNS ({})",
                    program.len(),
                    BPF_MAXINSNS
                ),
            ));
        }
        Ok(program)
    }
}

#[cfg(target_os = "windows")]
mod win {
    use super::*;
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
    use windows::Win32::Security::{
        CREATE_RESTRICTED_TOKEN_FLAGS, CreateRestrictedToken, CreateWellKnownSid,
        IsTokenRestricted, PSID, SID_AND_ATTRIBUTES, TOKEN_DUPLICATE, TOKEN_QUERY,
        WinRestrictedCodeSid,
    };
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetProcessMitigationPolicy, OpenProcessToken, PROCESS_MITIGATION_POLICY,
        ProcessChildProcessPolicy, ProcessDynamicCodePolicy, ProcessExtensionPointDisablePolicy,
        ProcessImageLoadPolicy, ProcessStrictHandleCheckPolicy, SetProcessMitigationPolicy,
    };
    use windows::core::{s, w};

    const JOB_OBJECT_BASIC_UI_RESTRICTIONS_CLASS: i32 = 4;
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS: i32 = 9;
    const SECURITY_MAX_SID_SIZE: usize = 68;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct JobObjectBasicLimitInformation {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct IoCounters {
        read_operation_count: u64,
        write_operation_count: u64,
        other_operation_count: u64,
        read_transfer_count: u64,
        write_transfer_count: u64,
        other_transfer_count: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct JobObjectExtendedLimitInformation {
        basic_limit_information: JobObjectBasicLimitInformation,
        io_info: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct JobObjectBasicUiRestrictions {
        ui_restrictions_class: u32,
    }

    #[cfg(target_pointer_width = "64")]
    const _: () = {
        assert!(size_of::<JobObjectBasicLimitInformation>() == 64);
        assert!(size_of::<IoCounters>() == 48);
        assert!(size_of::<JobObjectExtendedLimitInformation>() == 144);
        assert!(size_of::<JobObjectBasicUiRestrictions>() == 4);
    };

    type FarProc = unsafe extern "system" fn() -> isize;
    type CreateJobObjectWFn = unsafe extern "system" fn(*const c_void, *const u16) -> *mut c_void;
    type SetInformationJobObjectFn =
        unsafe extern "system" fn(*mut c_void, i32, *const c_void, u32) -> i32;
    type QueryInformationJobObjectFn =
        unsafe extern "system" fn(*mut c_void, i32, *mut c_void, u32, *mut u32) -> i32;
    type AssignProcessToJobObjectFn = unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;
    type TerminateJobObjectFn = unsafe extern "system" fn(*mut c_void, u32) -> i32;

    struct JobObjectApi {
        create: CreateJobObjectWFn,
        set_info: SetInformationJobObjectFn,
        query_info: QueryInformationJobObjectFn,
        assign: AssignProcessToJobObjectFn,
        terminate: TerminateJobObjectFn,
    }

    fn job_api() -> Result<&'static JobObjectApi, SandboxError> {
        static API: OnceLock<Option<JobObjectApi>> = OnceLock::new();
        API.get_or_init(|| {
            // safety: kernel32.dll is permanently loaded in every windows process and each export
            // matches its documented jobapi2.h / winbase.h signature
            unsafe {
                let module = GetModuleHandleW(w!("kernel32.dll")).ok()?;
                let create = GetProcAddress(module, s!("CreateJobObjectW"))?;
                let set_info = GetProcAddress(module, s!("SetInformationJobObject"))?;
                let query_info = GetProcAddress(module, s!("QueryInformationJobObject"))?;
                let assign = GetProcAddress(module, s!("AssignProcessToJobObject"))?;
                let terminate = GetProcAddress(module, s!("TerminateJobObject"))?;
                Some(JobObjectApi {
                    create: std::mem::transmute::<FarProc, CreateJobObjectWFn>(create),
                    set_info: std::mem::transmute::<FarProc, SetInformationJobObjectFn>(set_info),
                    query_info: std::mem::transmute::<FarProc, QueryInformationJobObjectFn>(
                        query_info,
                    ),
                    assign: std::mem::transmute::<FarProc, AssignProcessToJobObjectFn>(assign),
                    terminate: std::mem::transmute::<FarProc, TerminateJobObjectFn>(terminate),
                })
            }
        })
        .as_ref()
        .ok_or_else(|| SandboxError::new("job_object_api", "kernel32 job object exports missing"))
    }

    fn last_win32_error(operation: &'static str) -> SandboxError {
        // safety: reads the current thread's win32 last-error slot
        let code = unsafe { GetLastError().0 };
        SandboxError::new(operation, format!("win32 error {code}"))
    }

    pub struct JobObject {
        handle: HANDLE,
    }

    // safety: windows kernel job handles are thread-safe kernel objects
    unsafe impl Send for JobObject {}
    unsafe impl Sync for JobObject {}

    impl JobObject {
        pub fn new() -> Result<Self, SandboxError> {
            let api = job_api()?;
            // safety: null security attributes and null name create an anonymous job object
            let raw = unsafe { (api.create)(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(last_win32_error("CreateJobObjectW"));
            }
            Ok(Self {
                handle: HANDLE(raw),
            })
        }

        pub fn with_limits(limits: &JobLimits) -> Result<Self, SandboxError> {
            let job = Self::new()?;
            job.set_limits(limits)?;
            Ok(job)
        }

        pub fn set_limits(&self, limits: &JobLimits) -> Result<(), SandboxError> {
            let api = job_api()?;
            let mut info = JobObjectExtendedLimitInformation::default();
            info.basic_limit_information.limit_flags = limits.basic_limit_flags();
            if let Some(active) = limits.active_process_limit {
                info.basic_limit_information.active_process_limit = active;
            }
            if let Some(process_mem) = limits.process_memory_limit {
                info.process_memory_limit = process_mem;
            }
            if let Some(job_mem) = limits.job_memory_limit {
                info.job_memory_limit = job_mem;
            }
            // safety: info matches extended limit information abi and size
            let ok = unsafe {
                (api.set_info)(
                    self.handle.0,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    (&raw const info).cast(),
                    size_of::<JobObjectExtendedLimitInformation>() as u32,
                )
            };
            if ok == 0 {
                return Err(last_win32_error("SetInformationJobObject(extended_limits)"));
            }

            let ui = JobObjectBasicUiRestrictions {
                ui_restrictions_class: limits.ui_restrictions.raw_bits(),
            };
            // safety: ui matches basic ui restrictions abi and size
            let ok = unsafe {
                (api.set_info)(
                    self.handle.0,
                    JOB_OBJECT_BASIC_UI_RESTRICTIONS_CLASS,
                    (&raw const ui).cast(),
                    size_of::<JobObjectBasicUiRestrictions>() as u32,
                )
            };
            if ok == 0 {
                return Err(last_win32_error("SetInformationJobObject(ui_restrictions)"));
            }
            Ok(())
        }

        pub fn query_limits(&self) -> Result<JobLimits, SandboxError> {
            let api = job_api()?;
            let mut info = JobObjectExtendedLimitInformation::default();
            let mut returned = 0u32;
            // safety: info buffer matches extended limit information abi and size
            let ok = unsafe {
                (api.query_info)(
                    self.handle.0,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    (&raw mut info).cast(),
                    size_of::<JobObjectExtendedLimitInformation>() as u32,
                    &raw mut returned,
                )
            };
            if ok == 0 {
                return Err(last_win32_error(
                    "QueryInformationJobObject(extended_limits)",
                ));
            }

            let mut ui = JobObjectBasicUiRestrictions::default();
            let mut ui_returned = 0u32;
            // safety: ui buffer matches basic ui restrictions abi and size
            let ok = unsafe {
                (api.query_info)(
                    self.handle.0,
                    JOB_OBJECT_BASIC_UI_RESTRICTIONS_CLASS,
                    (&raw mut ui).cast(),
                    size_of::<JobObjectBasicUiRestrictions>() as u32,
                    &raw mut ui_returned,
                )
            };
            if ok == 0 {
                return Err(last_win32_error(
                    "QueryInformationJobObject(ui_restrictions)",
                ));
            }

            let flags = info.basic_limit_information.limit_flags;
            Ok(JobLimits {
                active_process_limit: (flags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS != 0)
                    .then_some(info.basic_limit_information.active_process_limit),
                process_memory_limit: (flags & JOB_OBJECT_LIMIT_PROCESS_MEMORY != 0)
                    .then_some(info.process_memory_limit),
                job_memory_limit: (flags & JOB_OBJECT_LIMIT_JOB_MEMORY != 0)
                    .then_some(info.job_memory_limit),
                kill_on_close: flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0,
                die_on_unhandled_exception: flags & JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
                    != 0,
                ui_restrictions: UiRestrictions::from_raw_bits(ui.ui_restrictions_class),
            })
        }

        pub fn assign_process(&self, process: HANDLE) -> Result<(), SandboxError> {
            let api = job_api()?;
            // safety: caller passes a valid windows process handle
            let ok = unsafe { (api.assign)(self.handle.0, process.0) };
            if ok == 0 {
                return Err(last_win32_error("AssignProcessToJobObject"));
            }
            Ok(())
        }

        pub fn assign_current_process(&self) -> Result<(), SandboxError> {
            // safety: current process pseudo-handle is valid for the calling process
            let current = unsafe { GetCurrentProcess() };
            self.assign_process(current)
        }

        pub fn terminate(&self, exit_code: u32) -> Result<(), SandboxError> {
            let api = job_api()?;
            // safety: self.handle is an owned open job object handle
            let ok = unsafe { (api.terminate)(self.handle.0, exit_code) };
            if ok == 0 {
                return Err(last_win32_error("TerminateJobObject"));
            }
            Ok(())
        }

        pub fn raw_handle(&self) -> HANDLE {
            self.handle
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            if !self.handle.is_invalid() {
                // safety: handle was created by job creation and is owned solely by self
                let _ = unsafe { CloseHandle(self.handle) };
            }
        }
    }

    fn set_u32_mitigation(
        policy: PROCESS_MITIGATION_POLICY,
        flags: u32,
        name: &'static str,
    ) -> Result<(), SandboxError> {
        if flags == 0 {
            return Ok(());
        }
        // safety: every supported policy struct in winnt.h is a 4-byte dword flags union
        unsafe { SetProcessMitigationPolicy(policy, (&raw const flags).cast(), size_of::<u32>()) }
            .map_err(|error| SandboxError::new(name, error.to_string()))
    }

    fn get_u32_mitigation(
        process: HANDLE,
        policy: PROCESS_MITIGATION_POLICY,
        name: &'static str,
    ) -> Result<u32, SandboxError> {
        let mut flags = 0u32;
        // safety: flags buffer matches the 4-byte policy layout for these mitigation classes
        unsafe {
            GetProcessMitigationPolicy(process, policy, (&raw mut flags).cast(), size_of::<u32>())
        }
        .map_err(|error| SandboxError::new(name, error.to_string()))?;
        Ok(flags)
    }

    pub fn apply_process_mitigations(policy: &MitigationPolicy) -> Result<(), SandboxError> {
        set_u32_mitigation(
            ProcessDynamicCodePolicy,
            policy.dynamic_code_flags(),
            "SetProcessMitigationPolicy(dynamic_code)",
        )?;
        set_u32_mitigation(
            ProcessStrictHandleCheckPolicy,
            policy.strict_handle_flags(),
            "SetProcessMitigationPolicy(strict_handle_check)",
        )?;
        set_u32_mitigation(
            ProcessExtensionPointDisablePolicy,
            policy.extension_point_flags(),
            "SetProcessMitigationPolicy(extension_point_disable)",
        )?;
        set_u32_mitigation(
            ProcessImageLoadPolicy,
            policy.image_load_flags(),
            "SetProcessMitigationPolicy(image_load)",
        )?;
        set_u32_mitigation(
            ProcessChildProcessPolicy,
            policy.child_process_flags(),
            "SetProcessMitigationPolicy(child_process)",
        )?;
        Ok(())
    }

    pub fn query_process_mitigations() -> Result<MitigationStatus, SandboxError> {
        // safety: GetCurrentProcess returns a pseudo-handle valid for the calling process
        let process = unsafe { GetCurrentProcess() };
        let dynamic = get_u32_mitigation(
            process,
            ProcessDynamicCodePolicy,
            "GetProcessMitigationPolicy(dynamic_code)",
        )?;
        let handles = get_u32_mitigation(
            process,
            ProcessStrictHandleCheckPolicy,
            "GetProcessMitigationPolicy(strict_handle_check)",
        )?;
        let ext = get_u32_mitigation(
            process,
            ProcessExtensionPointDisablePolicy,
            "GetProcessMitigationPolicy(extension_point_disable)",
        )?;
        let image = get_u32_mitigation(
            process,
            ProcessImageLoadPolicy,
            "GetProcessMitigationPolicy(image_load)",
        )?;
        let child = get_u32_mitigation(
            process,
            ProcessChildProcessPolicy,
            "GetProcessMitigationPolicy(child_process)",
        )?;
        Ok(MitigationStatus {
            prohibit_dynamic_code: dynamic & MITIGATION_PROHIBIT_DYNAMIC_CODE != 0,
            strict_handle_checks: handles & MITIGATION_STRICT_HANDLE_RAISE != 0,
            disable_extension_points: ext & MITIGATION_DISABLE_EXTENSION_POINTS != 0,
            no_remote_images: image & MITIGATION_NO_REMOTE_IMAGES != 0,
            no_low_label_images: image & MITIGATION_NO_LOW_LABEL_IMAGES != 0,
            prefer_system32_images: image & MITIGATION_PREFER_SYSTEM32_IMAGES != 0,
            no_child_process_creation: child & MITIGATION_NO_CHILD_PROCESS_CREATION != 0,
        })
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // safety: wrapper exclusively owns the handle passed at construction
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
    }

    pub struct RestrictedToken {
        handle: HANDLE,
    }

    // safety: windows access token handles are thread-safe kernel objects
    unsafe impl Send for RestrictedToken {}
    unsafe impl Sync for RestrictedToken {}

    impl RestrictedToken {
        pub fn from_current_process(config: &RestrictedTokenConfig) -> Result<Self, SandboxError> {
            let mut raw_token = HANDLE::default();
            // safety: current process pseudo-handle is valid and receives a new handle
            unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_DUPLICATE | TOKEN_QUERY,
                    &raw mut raw_token,
                )
            }
            .map_err(|error| SandboxError::new("OpenProcessToken", error.to_string()))?;
            let source = OwnedHandle(raw_token);

            // align sid storage on u32 as required for windows sid structs
            let mut sid_storage = [0u32; SECURITY_MAX_SID_SIZE / 4];
            let mut restricting = [SID_AND_ATTRIBUTES::default(); 1];
            let restricting_slice = if config.restrict_to_restricted_code_sid {
                let mut sid_len = size_of_val(&sid_storage) as u32;
                let psid = PSID(sid_storage.as_mut_ptr().cast());
                // safety: sid_storage is aligned and sized to the maximum sid byte length
                unsafe {
                    CreateWellKnownSid(WinRestrictedCodeSid, None, Some(psid), &raw mut sid_len)
                }
                .map_err(|error| SandboxError::new("CreateWellKnownSid", error.to_string()))?;
                restricting[0] = SID_AND_ATTRIBUTES {
                    Sid: psid,
                    Attributes: 0,
                };
                Some(&restricting[..])
            } else {
                None
            };

            let mut restricted = HANDLE::default();
            // safety: source handle has duplicate access and sid_storage lives across the call
            unsafe {
                CreateRestrictedToken(
                    source.0,
                    CREATE_RESTRICTED_TOKEN_FLAGS(config.raw_flags()),
                    None,
                    None,
                    restricting_slice,
                    &raw mut restricted,
                )
            }
            .map_err(|error| SandboxError::new("CreateRestrictedToken", error.to_string()))?;

            Ok(Self { handle: restricted })
        }

        pub fn is_restricted(&self) -> bool {
            // safety: self.handle is an owned open token handle
            unsafe { IsTokenRestricted(self.handle) }.is_ok()
        }

        pub fn raw_handle(&self) -> HANDLE {
            self.handle
        }
    }

    impl Drop for RestrictedToken {
        fn drop(&mut self) {
            if !self.handle.is_invalid() {
                // safety: handle was created as a restricted token and is owned solely by self
                let _ = unsafe { CloseHandle(self.handle) };
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use win::{JobObject, RestrictedToken, apply_process_mitigations, query_process_mitigations};

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::io;

    const PR_GET_DUMPABLE: i32 = 3;
    const PR_SET_DUMPABLE: i32 = 4;
    const PR_SET_SECCOMP: i32 = 22;
    const PR_SET_NO_NEW_PRIVS: i32 = 38;
    const PR_GET_NO_NEW_PRIVS: i32 = 39;

    const SECCOMP_MODE_FILTER: usize = 2;

    const RLIMIT_CORE: i32 = 4;
    const RLIMIT_NPROC: i32 = 6;
    const RLIMIT_NOFILE: i32 = 7;
    const RLIMIT_AS: i32 = 9;
    const RLIM_INFINITY: u64 = u64::MAX;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct RLimit {
        rlim_cur: u64,
        rlim_max: u64,
    }

    unsafe extern "C" {
        fn prctl(option: i32, ...) -> i32;
        fn getrlimit(resource: i32, rlim: *mut RLimit) -> i32;
        fn setrlimit(resource: i32, rlim: *const RLimit) -> i32;
    }

    fn os_error(operation: &'static str) -> SandboxError {
        SandboxError::new(operation, io::Error::last_os_error().to_string())
    }

    pub fn set_no_new_privs() -> Result<(), SandboxError> {
        // safety: no_new_privs prctl takes four trailing integer arguments (1, 0, 0, 0)
        let rc = unsafe { prctl(PR_SET_NO_NEW_PRIVS, 1usize, 0usize, 0usize, 0usize) };
        if rc != 0 {
            return Err(os_error("prctl(PR_SET_NO_NEW_PRIVS)"));
        }
        Ok(())
    }

    pub fn no_new_privs() -> Result<bool, SandboxError> {
        // safety: queries the calling thread's no_new_privs bit
        let rc = unsafe { prctl(PR_GET_NO_NEW_PRIVS, 0usize, 0usize, 0usize, 0usize) };
        if rc < 0 {
            return Err(os_error("prctl(PR_GET_NO_NEW_PRIVS)"));
        }
        Ok(rc == 1)
    }

    pub fn set_dumpable(dumpable: bool) -> Result<(), SandboxError> {
        // safety: dumpable prctl accepts 0 or 1 as its second argument
        let rc = unsafe {
            prctl(
                PR_SET_DUMPABLE,
                usize::from(dumpable),
                0usize,
                0usize,
                0usize,
            )
        };
        if rc != 0 {
            return Err(os_error("prctl(PR_SET_DUMPABLE)"));
        }
        Ok(())
    }

    pub fn is_dumpable() -> Result<bool, SandboxError> {
        // safety: returns the dumpable flag or -1 on error
        let rc = unsafe { prctl(PR_GET_DUMPABLE, 0usize, 0usize, 0usize, 0usize) };
        if rc < 0 {
            return Err(os_error("prctl(PR_GET_DUMPABLE)"));
        }
        Ok(rc != 0)
    }

    fn cap_rlimit(resource: i32, cap: u64, name: &'static str) -> Result<(), SandboxError> {
        let mut current = RLimit::default();
        // safety: current is a valid writable rlimit struct
        if unsafe { getrlimit(resource, &raw mut current) } != 0 {
            return Err(os_error(name));
        }
        let target = cap.min(current.rlim_max);
        let next = RLimit {
            rlim_cur: target.min(current.rlim_cur),
            rlim_max: target,
        };
        // safety: next only lowers or preserves rlim_max so unprivileged callers may apply it
        if unsafe { setrlimit(resource, &raw const next) } != 0 {
            return Err(os_error(name));
        }
        Ok(())
    }

    fn read_rlimit(resource: i32, name: &'static str) -> Result<Option<u64>, SandboxError> {
        let mut current = RLimit::default();
        // safety: current is a valid writable rlimit struct
        if unsafe { getrlimit(resource, &raw mut current) } != 0 {
            return Err(os_error(name));
        }
        Ok((current.rlim_cur != RLIM_INFINITY).then_some(current.rlim_cur))
    }

    pub fn apply_rlimits(limits: &ResourceLimits) -> Result<(), SandboxError> {
        if let Some(core) = limits.core_bytes {
            cap_rlimit(RLIMIT_CORE, core, "setrlimit(RLIMIT_CORE)")?;
        }
        if let Some(nofile) = limits.max_open_files {
            cap_rlimit(RLIMIT_NOFILE, nofile, "setrlimit(RLIMIT_NOFILE)")?;
        }
        if let Some(as_bytes) = limits.address_space_bytes {
            cap_rlimit(RLIMIT_AS, as_bytes, "setrlimit(RLIMIT_AS)")?;
        }
        if let Some(nproc) = limits.max_processes {
            cap_rlimit(RLIMIT_NPROC, nproc, "setrlimit(RLIMIT_NPROC)")?;
        }
        Ok(())
    }

    pub fn query_rlimits() -> Result<ResourceLimits, SandboxError> {
        Ok(ResourceLimits {
            core_bytes: read_rlimit(RLIMIT_CORE, "getrlimit(RLIMIT_CORE)")?,
            max_open_files: read_rlimit(RLIMIT_NOFILE, "getrlimit(RLIMIT_NOFILE)")?,
            address_space_bytes: read_rlimit(RLIMIT_AS, "getrlimit(RLIMIT_AS)")?,
            max_processes: read_rlimit(RLIMIT_NPROC, "getrlimit(RLIMIT_NPROC)")?,
        })
    }

    impl SeccompFilter {
        pub fn install(&self) -> Result<(), SandboxError> {
            let current = SeccompArch::current().ok_or_else(|| {
                SandboxError::new("seccomp_install", "unsupported target architecture")
            })?;
            if self.arch != current {
                return Err(SandboxError::new(
                    "seccomp_install",
                    "filter architecture does not match host architecture",
                ));
            }
            let instructions = self.compile()?;
            set_no_new_privs()?;
            let prog = SockFprog {
                len: instructions.len() as u16,
                filter: instructions.as_ptr(),
            };
            // safety: prog points to instructions which stays live for the duration of prctl
            let rc = unsafe {
                prctl(
                    PR_SET_SECCOMP,
                    SECCOMP_MODE_FILTER,
                    (&raw const prog) as usize,
                    0usize,
                    0usize,
                )
            };
            if rc != 0 {
                return Err(os_error("prctl(PR_SET_SECCOMP)"));
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{
    apply_rlimits, is_dumpable, no_new_privs, query_rlimits, set_dumpable, set_no_new_privs,
};

#[cfg(test)]
mod tests {
    use super::*;

    // minimal classic bpf evaluator matching linux kernel seccomp_data evaluation
    fn eval_bpf(program: &[SockFilter], arch: u32, nr: u32) -> u32 {
        let mut pc = 0usize;
        let mut acc = 0u32;
        loop {
            let insn = program[pc];
            match insn.code {
                c if c == BPF_LD | BPF_W | BPF_ABS => {
                    acc = match insn.k {
                        SECCOMP_DATA_NR_OFFSET => nr,
                        SECCOMP_DATA_ARCH_OFFSET => arch,
                        other => panic!("unexpected seccomp_data offset {other}"),
                    };
                    pc += 1;
                }
                c if c == BPF_JMP | BPF_JEQ | BPF_K => {
                    let jump = if acc == insn.k { insn.jt } else { insn.jf };
                    pc += 1 + usize::from(jump);
                }
                c if c == BPF_RET | BPF_K => return insn.k,
                other => panic!("unexpected bpf opcode 0x{other:x}"),
            }
        }
    }

    #[test]
    fn job_limits_and_ui_restrictions_roundtrip_bits() {
        let ui = UiRestrictions::renderer_default();
        assert_eq!(ui.raw_bits(), 0xFF);
        assert_eq!(UiRestrictions::from_raw_bits(ui.raw_bits()), ui);

        let partial = UiRestrictions {
            handles: true,
            read_clipboard: true,
            desktop: true,
            ..UiRestrictions::default()
        };
        assert_eq!(UiRestrictions::from_raw_bits(partial.raw_bits()), partial);

        let limits = JobLimits::renderer_default();
        assert_eq!(
            limits.basic_limit_flags(),
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
        );
    }

    #[test]
    fn mitigation_policy_preserves_jit_when_requested() {
        let with_jit = MitigationPolicy::renderer_default(true);
        assert_eq!(with_jit.dynamic_code_flags(), 0);
        assert_eq!(
            with_jit.strict_handle_flags(),
            MITIGATION_STRICT_HANDLE_RAISE | MITIGATION_STRICT_HANDLE_PERMANENT
        );
        assert_eq!(
            with_jit.extension_point_flags(),
            MITIGATION_DISABLE_EXTENSION_POINTS
        );
        assert_eq!(
            with_jit.image_load_flags(),
            MITIGATION_NO_REMOTE_IMAGES
                | MITIGATION_NO_LOW_LABEL_IMAGES
                | MITIGATION_PREFER_SYSTEM32_IMAGES
        );
        assert_eq!(
            with_jit.child_process_flags(),
            MITIGATION_NO_CHILD_PROCESS_CREATION
        );

        let no_jit = MitigationPolicy::renderer_default(false);
        assert_eq!(
            no_jit.dynamic_code_flags(),
            MITIGATION_PROHIBIT_DYNAMIC_CODE
        );
    }

    #[test]
    fn seccomp_filter_bytecode_enforces_arch_allowlist_and_overrides() {
        let filter = SeccompFilter::new(SeccompArch::X86_64, SeccompAction::KillProcess)
            .allow_many(&[0, 1, 60, 231])
            .rule(63, SeccompAction::Errno(1))
            .rule(39, SeccompAction::Trap);
        let program = filter.compile().expect("filter should compile");

        // wrong architecture always kills immediately regardless of syscall number
        assert_eq!(
            eval_bpf(&program, AUDIT_ARCH_AARCH64, 0),
            SeccompAction::KillProcess.raw()
        );
        assert_eq!(
            eval_bpf(&program, AUDIT_ARCH_AARCH64, 231),
            SeccompAction::KillProcess.raw()
        );

        for allowed in [0, 1, 60, 231] {
            assert_eq!(
                eval_bpf(&program, AUDIT_ARCH_X86_64, allowed),
                SeccompAction::Allow.raw()
            );
        }
        assert_eq!(
            eval_bpf(&program, AUDIT_ARCH_X86_64, 63),
            SeccompAction::Errno(1).raw()
        );
        assert_eq!(
            eval_bpf(&program, AUDIT_ARCH_X86_64, 39),
            SeccompAction::Trap.raw()
        );
        assert_eq!(
            eval_bpf(&program, AUDIT_ARCH_X86_64, 59),
            SeccompAction::KillProcess.raw()
        );
    }

    #[test]
    fn seccomp_filter_chunks_more_than_255_rules_without_jump_overflow() {
        let mut filter = SeccompFilter::new(SeccompArch::X86_64, SeccompAction::Errno(1));
        for nr in 0..320u32 {
            filter = filter.allow(nr);
        }
        let program = filter.compile().expect("large filter should compile");
        for nr in 0..320u32 {
            assert_eq!(
                eval_bpf(&program, AUDIT_ARCH_X86_64, nr),
                SeccompAction::Allow.raw(),
                "syscall {nr} should be allowed"
            );
        }
        assert_eq!(
            eval_bpf(&program, AUDIT_ARCH_X86_64, 320),
            SeccompAction::Errno(1).raw()
        );
    }

    #[test]
    fn seccomp_filter_rejects_programs_exceeding_bpf_maxinsns() {
        let mut filter = SeccompFilter::new(SeccompArch::X86_64, SeccompAction::KillProcess);
        for nr in 0..4200u32 {
            filter = filter.allow(nr);
        }
        let error = filter.compile().expect_err("oversized filter must error");
        assert_eq!(error.operation(), "seccomp_compile");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_job_object_applies_and_queries_limits() {
        let limits = JobLimits {
            active_process_limit: Some(2),
            process_memory_limit: Some(128 * 1024 * 1024),
            job_memory_limit: Some(256 * 1024 * 1024),
            kill_on_close: true,
            die_on_unhandled_exception: true,
            ui_restrictions: UiRestrictions::renderer_default(),
        };
        let job = JobObject::with_limits(&limits).expect("job object should initialize");
        assert!(!job.raw_handle().is_invalid());
        let queried = job.query_limits().expect("job limits should query back");
        assert_eq!(queried, limits);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_process_mitigations_query_and_apply_safe_image_policy() {
        let before = query_process_mitigations().expect("mitigation query should succeed");
        // only apply non-destructive image load hardening on the shared test process so jit and
        // child-process tests elsewhere in the workspace stay unaffected
        let safe = MitigationPolicy {
            no_remote_images: true,
            prefer_system32_images: true,
            ..MitigationPolicy::default()
        };
        apply_process_mitigations(&safe).expect("image load mitigation should apply");
        let after = query_process_mitigations().expect("mitigation query should succeed");
        assert!(after.no_remote_images);
        assert!(after.prefer_system32_images);
        assert_eq!(after.prohibit_dynamic_code, before.prohibit_dynamic_code);
        assert_eq!(
            after.no_child_process_creation,
            before.no_child_process_creation
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_restricted_token_creates_and_reports_restricted_status() {
        let restricted =
            RestrictedToken::from_current_process(&RestrictedTokenConfig::renderer_default())
                .expect("restricted token should be created");
        assert!(!restricted.raw_handle().is_invalid());
        assert!(restricted.is_restricted());

        let priv_only = RestrictedToken::from_current_process(&RestrictedTokenConfig {
            disable_max_privilege: true,
            restrict_to_restricted_code_sid: false,
            ..RestrictedTokenConfig::default()
        })
        .expect("privilege-stripped token should be created");
        assert!(!priv_only.raw_handle().is_invalid());
        // win32 restriction check reports presence of restricting sids only
        assert!(!priv_only.is_restricted());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_child_process_enforces_no_new_privs_rlimits_and_seccomp_filter() {
        const EPERM: i32 = 1;
        #[cfg(target_arch = "x86_64")]
        const SYS_GETPID: u32 = 39;
        #[cfg(target_arch = "x86_64")]
        const SYS_UNAME: u32 = 63;
        #[cfg(target_arch = "x86_64")]
        const SYS_EXIT: u32 = 60;
        #[cfg(target_arch = "x86_64")]
        const SYS_EXIT_GROUP: u32 = 231;

        #[cfg(target_arch = "aarch64")]
        const SYS_GETPID: u32 = 172;
        #[cfg(target_arch = "aarch64")]
        const SYS_UNAME: u32 = 160;
        #[cfg(target_arch = "aarch64")]
        const SYS_EXIT: u32 = 93;
        #[cfg(target_arch = "aarch64")]
        const SYS_EXIT_GROUP: u32 = 94;

        unsafe extern "C" {
            fn fork() -> i32;
            fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
            fn syscall(number: isize, ...) -> isize;
            fn __errno_location() -> *mut i32;
        }

        // safety: fork isolates seccomp and prctl state mutations to a dedicated child process
        let pid = unsafe { fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            let exit_now = |code: isize| -> ! {
                // safety: exit_group is explicitly allowed by the child filter
                unsafe {
                    syscall(SYS_EXIT_GROUP as isize, code);
                }
                loop {
                    std::hint::spin_loop();
                }
            };

            if set_dumpable(false).is_err() || is_dumpable().unwrap_or(true) {
                exit_now(10);
            }
            if apply_rlimits(&ResourceLimits::renderer_default()).is_err() {
                exit_now(11);
            }
            match query_rlimits() {
                Ok(limits) if limits.core_bytes == Some(0) => {}
                _ => exit_now(12),
            }

            let filter = match SeccompFilter::for_current_arch(SeccompAction::Errno(EPERM as u16)) {
                Ok(f) => f.allow_many(&[SYS_GETPID, SYS_EXIT, SYS_EXIT_GROUP]),
                Err(_) => exit_now(13),
            };
            if filter.install().is_err() {
                exit_now(14);
            }
            // allowlist permits getpid
            // safety: getpid takes no arguments and is in the installed allowlist
            let child_pid = unsafe { syscall(SYS_GETPID as isize) };
            if child_pid <= 0 {
                exit_now(15);
            }
            // denied syscall must fail with eperm rather than reaching the kernel handler
            let mut uts = [0u8; 512];
            // safety: uname buffer is large enough for struct utsname even if allowed
            let rc = unsafe { syscall(SYS_UNAME as isize, uts.as_mut_ptr()) };
            // safety: __errno_location returns the calling thread's errno slot
            let err = unsafe { *__errno_location() };
            if rc != -1 || err != EPERM {
                exit_now(16);
            }
            exit_now(0);
        }

        let mut status = 0i32;
        // safety: waits on the child pid returned by fork
        let waited = unsafe { waitpid(pid, &raw mut status, 0) };
        assert_eq!(waited, pid);
        let exited = (status & 0x7f) == 0;
        let exit_code = (status >> 8) & 0xff;
        assert!(
            exited && exit_code == 0,
            "child sandbox check failed (status=0x{status:x}, code={exit_code})"
        );
    }
}
