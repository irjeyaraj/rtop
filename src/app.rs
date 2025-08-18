use std::time::Instant;

/// Global application state shared between the draw loop and input handler.
///
/// Fields capture the current UI selection and popup states, as well as cached
/// detection results to reduce per-frame workload.
pub struct App {
    pub selected_top_tab: usize, // 0: Dashboard, 1: top/htop, 2: Services, 3: Shell, 4: Logs
    #[allow(dead_code)]
    pub selected_proc_tab: usize, // reserved (no Process tab)
    // Help popup state
    pub help_popup: bool,
    // Network rate tracking (iface -> (rx_bytes, tx_bytes) and computed rates in bytes/sec)
    pub net_prev: std::collections::HashMap<String, (u64, u64)>,
    pub net_rates: std::collections::HashMap<String, (f64, f64)>,
    pub net_last: Instant,
    // Cached GPU detection (best-effort; computed once on startup)
    pub gpus: Vec<super::GpuInfo>,
    // Embedded shell session (PTY) for the Shell tab
    pub shell: Option<super::shell::ShellSession>,
    // Services tab state
    pub services_scroll: usize, // top visible row index
    pub services_selected: usize, // absolute selected row index
    // Service details popup state
    pub service_popup: bool,
    pub service_detail_title: String,
    pub service_detail_text: String,
    // Processes (top/htop) tab state
    pub procs_scroll: usize, // top visible row index
    pub procs_selected: usize, // absolute selected row index among sorted list
    // Process details popup state
    pub process_popup: bool,
    pub process_detail_title: String,
    pub process_detail_text: String,
    // Cached list of process PIDs sorted by CPU (rebuilt each tick)
    pub procs_pids_sorted: Vec<i32>,
    // Logs tab state
    pub logs_scroll: usize,
    pub logs_selected: usize,
    // Journal tab state
    pub journal_scroll: usize,
    pub journal_selected: usize,
    // Log content popup state (reused for Journal)
    pub log_popup: bool,
    pub log_detail_title: String,
    pub log_detail_text: String,
    pub log_popup_scroll: usize,
    // Sudo password prompt state for Logs/Journal
    pub logs_password_prompt: bool,
    pub logs_password_input: String,
    pub logs_password_error: String,
    pub logs_sudo_password: Option<String>,
    pub logs_pending_path: String, // path awaiting sudo read (log or journal)
}

/// Construct the initial application state.
impl Default for App {
    fn default() -> Self {
        Self {
            selected_top_tab: 0,
            selected_proc_tab: 0,
            help_popup: false,
            net_prev: std::collections::HashMap::new(),
            net_rates: std::collections::HashMap::new(),
            net_last: Instant::now(),
            gpus: Vec::new(),
            shell: None,
            services_scroll: 0,
            services_selected: 0,
            service_popup: false,
            service_detail_title: String::new(),
            service_detail_text: String::new(),
            procs_scroll: 0,
            procs_selected: 0,
            process_popup: false,
            process_detail_title: String::new(),
            process_detail_text: String::new(),
            procs_pids_sorted: Vec::new(),
            logs_scroll: 0,
            logs_selected: 0,
            journal_scroll: 0,
            journal_selected: 0,
            log_popup: false,
            log_detail_title: String::new(),
            log_detail_text: String::new(),
            log_popup_scroll: 0,
            logs_password_prompt: false,
            logs_password_input: String::new(),
            logs_password_error: String::new(),
            logs_sudo_password: None,
            logs_pending_path: String::new(),
        }
    }
}
