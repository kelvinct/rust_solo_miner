use eframe::egui;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 700.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        "🦀 Rust SHA-256d Stratum Miner",
        options,
        Box::new(|cc| {
            setup_custom_fonts(&cc.egui_ctx);
            // egui 0.27：creator 回傳 Result<Box<dyn App>, ...>
            Ok(Box::new(MinerApp::default()))
        }),
    )
}

// egui 0.27：font_data 是 BTreeMap<String, FontData>，不需要 Arc
fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Ok(font_data) = std::fs::read("C:\\Windows\\Fonts\\msjh.ttc") {
        fonts.font_data.insert(
            "msjh".to_owned(),
            egui::FontData::from_owned(font_data),
        );
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "msjh".to_owned());
        fonts
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .unwrap()
            .insert(0, "msjh".to_owned());
        ctx.set_fonts(fonts);
    }
}

// ─────────────────────────────────────────────────────────────────
//  資料結構
// ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct JobInfo {
    job_id: String,
    prevhash: String,
    coinb1: String,
    coinb2: String,
    merkle_branches: Vec<String>,
    version: String,
    nbits: String,
    ntime: String,
    extranonce1: String,
    extranonce2_size: usize,
    target: [u8; 32],
}

#[derive(Clone, Debug)]
enum PoolEvent {
    Log(String),
    Subscribed {
        extranonce1: String,
        extranonce2_size: usize,
    },
    Authorized,
    Difficulty(f64),
    NewJob(Box<JobInfo>),
    ShareResult {
        accepted: bool,
        error: Option<String>,
    },
}

// ─────────────────────────────────────────────────────────────────
//  GUI 應用程式
// ─────────────────────────────────────────────────────────────────

struct MinerApp {
    pool_host: String,
    pool_port: String,
    wallet_address: String,
    worker_name: String,
    cpu_threads: usize,

    is_running: Arc<AtomicBool>,
    total_hashes: Arc<AtomicU64>,
    current_hashrate: f64,
    last_reported_hashes: u64,
    last_report_time: Instant,

    event_rx: Option<mpsc::Receiver<PoolEvent>>,
    logs: Vec<String>,
    status: String,
    shares_accepted: u32,
    shares_rejected: u32,
    last_share_info: String,
}

impl Default for MinerApp {
    fn default() -> Self {
        Self {
            pool_host: "public-pool.io".to_string(),
            pool_port: "21496".to_string(),
            wallet_address: "bc1qm0whrhkmlx8znf07he4af206s8akw6jg08d5vh".to_string(),
            worker_name: "RustGUI_Perf".to_string(),
            cpu_threads: 16,
            is_running: Arc::new(AtomicBool::new(false)),
            total_hashes: Arc::new(AtomicU64::new(0)),
            current_hashrate: 0.0,
            last_reported_hashes: 0,
            last_report_time: Instant::now(),
            event_rx: None,
            logs: Vec::new(),
            status: "閒置".to_string(),
            shares_accepted: 0,
            shares_rejected: 0,
            last_share_info: String::new(),
        }
    }
}

impl eframe::App for MinerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(500));

        // ── 排空礦池事件 ──
        if let Some(rx) = &self.event_rx {
            while let Ok(evt) = rx.try_recv() {
                match evt {
                    PoolEvent::Log(msg) => {
                        self.logs.push(msg);
                        if self.logs.len() > 300 {
                            self.logs.remove(0);
                        }
                    }
                    PoolEvent::Subscribed {
                        extranonce1,
                        extranonce2_size,
                    } => {
                        self.status = format!(
                            "已訂閱 extranonce1={} extranonce2_size={}",
                            extranonce1, extranonce2_size
                        );
                    }
                    PoolEvent::Authorized => {
                        self.status = "已認證".to_string();
                    }
                    PoolEvent::Difficulty(d) => {
                        self.status = format!("難度 = {}", d);
                    }
                    PoolEvent::NewJob(job) => {
                        self.status = format!("新工作: {}", job.job_id);
                    }
                    PoolEvent::ShareResult { accepted, error } => {
                        if accepted {
                            self.shares_accepted += 1;
                            self.last_share_info = "✅ 礦池接受 share".to_string();
                        } else {
                            self.shares_rejected += 1;
                            self.last_share_info =
                                format!("❌ 礦池拒絕: {}", error.unwrap_or_default());
                        }
                    }
                }
            }
        }

        // ── 算力更新 ──
        if self.is_running.load(Ordering::Relaxed) {
            let elapsed = self.last_report_time.elapsed().as_secs_f64();
            if elapsed >= 2.0 {
                let current_total = self.total_hashes.load(Ordering::Relaxed);
                let diff = current_total.saturating_sub(self.last_reported_hashes);
                self.current_hashrate = (diff as f64 / elapsed) / 1000.0;
                self.last_reported_hashes = current_total;
                self.last_report_time = Instant::now();
            }
        } else {
            self.current_hashrate = 0.0;
        }

        // ── 繪製 GUI ──
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("🦀 Rust SHA-256d Stratum 礦機");
            });
            ui.separator();

            egui::Grid::new("config_grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("礦池位址:");
                    ui.text_edit_singleline(&mut self.pool_host);
                    ui.end_row();

                    ui.label("連接埠:");
                    ui.text_edit_singleline(&mut self.pool_port);
                    ui.end_row();

                    ui.label("比特幣錢包地址:");
                    ui.text_edit_singleline(&mut self.wallet_address);
                    ui.end_row();

                    ui.label("礦工自訂名稱:");
                    ui.text_edit_singleline(&mut self.worker_name);
                    ui.end_row();

                    ui.label("CPU 執行緒數:");
                    ui.add(egui::Slider::new(&mut self.cpu_threads, 1..=32));
                    ui.end_row();
                });

            ui.separator();
            ui.add_space(6.0);

            ui.vertical_centered(|ui| {
                let running_status = if self.is_running.load(Ordering::Relaxed) {
                    "🟢 正在挖礦中..."
                } else {
                    "🔴 已停止"
                };
                ui.label(egui::RichText::new(running_status).size(16.0).strong());

                ui.add_space(4.0);

                ui.label(
                    egui::RichText::new(format!("當前算力: {:.2} KH/s", self.current_hashrate))
                        .size(22.0)
                        .color(egui::Color32::from_rgb(0, 200, 100))
                        .strong(),
                );

                ui.label(
                    egui::RichText::new(format!(
                        "累計雜湊: {}",
                        self.total_hashes.load(Ordering::Relaxed)
                    ))
                    .size(12.0)
                    .color(egui::Color32::GRAY),
                );

                ui.add_space(4.0);

                ui.label(
                    egui::RichText::new(format!(
                        "✅ 接受 {} | ❌ 拒絕 {}",
                        self.shares_accepted, self.shares_rejected
                    ))
                    .size(13.0),
                );

                if !self.last_share_info.is_empty() {
                    ui.label(
                        egui::RichText::new(&self.last_share_info)
                            .size(12.0)
                            .color(egui::Color32::YELLOW),
                    );
                }

                ui.label(
                    egui::RichText::new(format!("狀態: {}", self.status))
                        .size(12.0)
                        .color(egui::Color32::LIGHT_BLUE),
                );
            });

            ui.add_space(8.0);
            ui.separator();

            // ── 開始 / 停止按鈕 ──
            ui.horizontal(|ui| {
                let start_btn = egui::Button::new(egui::RichText::new("🚀 啟動礦機").size(16.0));
                if ui
                    .add_enabled(!self.is_running.load(Ordering::Relaxed), start_btn)
                    .clicked()
                {
                    self.start_mining();
                }

                let stop_btn = egui::Button::new(egui::RichText::new("🛑 停止礦機").size(16.0));
                if ui
                    .add_enabled(self.is_running.load(Ordering::Relaxed), stop_btn)
                    .clicked()
                {
                    self.is_running.store(false, Ordering::SeqCst);
                    self.logs.push("[GUI] 停止指令已發出".to_string());
                }

                if ui.button("🗑 清除日誌").clicked() {
                    self.logs.clear();
                }
            });

            ui.add_space(6.0);
            ui.separator();
            ui.label("日誌:");
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.logs {
                        ui.label(egui::RichText::new(line).monospace().size(11.0));
                    }
                });
        });
    }
}

impl MinerApp {
    fn start_mining(&mut self) {
        self.logs.clear();
        self.shares_accepted = 0;
        self.shares_rejected = 0;
        self.last_share_info.clear();
        self.total_hashes.store(0, Ordering::Relaxed);
        self.last_reported_hashes = 0;
        self.last_report_time = Instant::now();
        self.status = "連線中...".to_string();

        let (tx, rx) = mpsc::channel();
        self.event_rx = Some(rx);

        let host = self.pool_host.clone();
        let port: u16 = self.pool_port.parse().unwrap_or(21496);
        let wallet = self.wallet_address.clone();
        let worker = self.worker_name.clone();
        let threads = self.cpu_threads;

        let is_running = Arc::clone(&self.is_running);
        let total_hashes = Arc::clone(&self.total_hashes);

        is_running.store(true, Ordering::SeqCst);

        thread::spawn(move || {
            pool_thread(
                host,
                port,
                wallet,
                worker,
                threads,
                is_running,
                total_hashes,
                tx,
            );
        });
    }
}

// ─────────────────────────────────────────────────────────────────
//  網路執行緒
// ─────────────────────────────────────────────────────────────────

fn pool_thread(
    host: String,
    port: u16,
    wallet: String,
    worker: String,
    cpu_threads: usize,
    is_running: Arc<AtomicBool>,
    total_hashes: Arc<AtomicU64>,
    event_tx: mpsc::Sender<PoolEvent>,
) {
    let log = |msg: String| {
        let _ = event_tx.send(PoolEvent::Log(msg));
    };

    log(format!("連線至 {}:{}...", host, port));

    let stream = match TcpStream::connect((host.as_str(), port)) {
        Ok(s) => s,
        Err(e) => {
            log(format!("連線失敗: {}", e));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));

    let read_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            log(format!("複製 socket 失敗: {}", e));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    let write_stream = Arc::new(Mutex::new(stream));
    let mut reader = BufReader::new(read_stream);

    // 送出 subscribe + authorize
    {
        let mut w = write_stream.lock().unwrap();
        let _ = w.write_all(
            b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"rust-gui-miner/1.0.0\"]}\n",
        );
        let auth = format!(
            "{{\"id\":2,\"method\":\"mining.authorize\",\"params\":[\"{}.{}\",\"x\"]}}\n",
            wallet, worker
        );
        let _ = w.write_all(auth.as_bytes());
        let _ = w.flush();
    }
    log("已送出 subscribe + authorize".to_string());

    let mut extranonce1 = String::new();
    let mut extranonce2_size = 4usize;
    let mut current_difficulty = 1.0f64;
    let mut current_target: [u8; 32] = target_from_difficulty(1.0);

    let submit_id_counter = Arc::new(AtomicU64::new(100));

    let mut worker_handles: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut current_stop: Option<Arc<AtomicBool>> = None;

    loop {
        if !is_running.load(Ordering::Relaxed) {
            break;
        }

        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                log("礦池關閉了連線 (EOF)".to_string());
                break;
            }
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let v: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        log(format!("無法解析 JSON: {} | 原始: {}", e, trimmed));
                        continue;
                    }
                };

                // ── 回應 (有 id) ──
                if let Some(id) = v.get("id").and_then(|x| x.as_u64()) {
                    if id == 1 {
                        if let Some(arr) = v.get("result").and_then(|r| r.as_array()) {
                            if arr.len() >= 3 {
                                if let Some(e1) = arr[1].as_str() {
                                    extranonce1 = e1.to_string();
                                }
                                if let Some(e2) = arr[2].as_u64() {
                                    extranonce2_size = e2 as usize;
                                }
                                let _ = event_tx.send(PoolEvent::Subscribed {
                                    extranonce1: extranonce1.clone(),
                                    extranonce2_size,
                                });
                                log(format!(
                                    "訂閱成功: extranonce1={}, extranonce2_size={}",
                                    extranonce1, extranonce2_size
                                ));
                            }
                        }
                    } else if id == 2 {
                        let ok = v
                            .get("result")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false);
                        if ok {
                            let _ = event_tx.send(PoolEvent::Authorized);
                            log("認證成功".to_string());
                        } else {
                            log(format!("認證失敗: {:?}", v.get("error")));
                            break;
                        }
                    } else if id >= 100 {
                        let ok = v
                            .get("result")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false);
                        let err = v
                            .get("error")
                            .filter(|e| !e.is_null())
                            .map(|e| e.to_string());
                        let _ = event_tx.send(PoolEvent::ShareResult {
                            accepted: ok,
                            error: err.clone(),
                        });
                        if ok {
                            log("✅ 礦池接受 share!".to_string());
                        } else {
                            log(format!("❌ 礦池拒絕 share: {:?}", err));
                        }
                    }
                    continue;
                }

                // ── 通知 (有 method) ──
                let method = v.get("method").and_then(|x| x.as_str()).unwrap_or("");
                let params = v.get("params").cloned().unwrap_or(Value::Null);

                match method {
                    "mining.set_difficulty" => {
                        if let Some(d) = params
                            .as_array()
                            .and_then(|a| a.get(0))
                            .and_then(|x| x.as_f64())
                        {
                            // 🔬 測試模式：想驗證 submit 格式就取消下面三行註解
                            // let d_effective = 0.01_f64;
                            // current_difficulty = d_effective;
                            // current_target = target_from_difficulty(d_effective);

                            current_difficulty = d;
                            current_target = target_from_difficulty(d);
                            let _ = event_tx.send(PoolEvent::Difficulty(d));
                            log(format!("難度設定為 {}", d));
                        }
                    }
                    "mining.set_extranonce" => {
                        if let Some(arr) = params.as_array() {
                            if let Some(e1) = arr.get(0).and_then(|x| x.as_str()) {
                                extranonce1 = e1.to_string();
                            }
                            if let Some(e2) = arr.get(1).and_then(|x| x.as_u64()) {
                                extranonce2_size = e2 as usize;
                            }
                            log(format!(
                                "Extranonce 更新: {} / {}",
                                extranonce1, extranonce2_size
                            ));
                        }
                    }
                    "mining.notify" => {
                        let arr = match params.as_array() {
                            Some(a) if a.len() >= 9 => a,
                            _ => continue,
                        };

                        if let Some(s) = current_stop.take() {
                            s.store(false, Ordering::SeqCst);
                        }
                        for h in worker_handles.drain(..) {
                            let _ = h.join();
                        }

                        let merkle_branches: Vec<String> = arr[4]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();

                        let job = JobInfo {
                            job_id: arr[0].as_str().unwrap_or("").to_string(),
                            prevhash: arr[1].as_str().unwrap_or("").to_string(),
                            coinb1: arr[2].as_str().unwrap_or("").to_string(),
                            coinb2: arr[3].as_str().unwrap_or("").to_string(),
                            merkle_branches,
                            version: arr[5].as_str().unwrap_or("").to_string(),
                            nbits: arr[6].as_str().unwrap_or("").to_string(),
                            ntime: arr[7].as_str().unwrap_or("").to_string(),
                            extranonce1: extranonce1.clone(),
                            extranonce2_size,
                            target: current_target,
                        };

                        log(format!(
                            "新工作: {} (diff={})",
                            job.job_id, current_difficulty
                        ));

                        let _ = event_tx.send(PoolEvent::NewJob(Box::new(job.clone())));

                        let job_stop = Arc::new(AtomicBool::new(true));
                        current_stop = Some(Arc::clone(&job_stop));

                        let job_arc = Arc::new(job);

                        for i in 0..cpu_threads {
                            let h_hashes = Arc::clone(&total_hashes);
                            let h_stop = Arc::clone(&job_stop);
                            let h_job = Arc::clone(&job_arc);
                            let h_write = Arc::clone(&write_stream);
                            let h_submit_ids = Arc::clone(&submit_id_counter);
                            let wallet_c = wallet.clone();
                            let worker_c = worker.clone();
                            let evt_tx = event_tx.clone();
                            let start_nonce = (i as u32).wrapping_mul(0x1000_0000);
                            let thread_index = i as u32;

                            let handle = thread::spawn(move || {
                                mining_worker(
                                    h_stop,
                                    h_hashes,
                                    h_job,
                                    h_write,
                                    thread_index,
                                    start_nonce,
                                    wallet_c,
                                    worker_c,
                                    evt_tx,
                                    h_submit_ids,
                                );
                            });
                            worker_handles.push(handle);
                        }
                    }
                    _ => {}
                }
            }
            Err(e) => match e.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                    continue;
                }
                _ => {
                    log(format!("讀取錯誤: {}", e));
                    break;
                }
            },
        }
    }

    if let Some(s) = current_stop.take() {
        s.store(false, Ordering::SeqCst);
    }
    for h in worker_handles.drain(..) {
        let _ = h.join();
    }

    is_running.store(false, Ordering::SeqCst);
    log("礦池執行緒結束".to_string());
}

// ─────────────────────────────────────────────────────────────────
//  挖礦執行緒
// ─────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn mining_worker(
    stop: Arc<AtomicBool>,
    total_hashes: Arc<AtomicU64>,
    job: Arc<JobInfo>,
    write_stream: Arc<Mutex<TcpStream>>,
    thread_index: u32,
    start_nonce: u32,
    wallet: String,
    worker: String,
    event_tx: mpsc::Sender<PoolEvent>,
    submit_id_counter: Arc<AtomicU64>,
) {
    let mut extranonce2 = vec![0u8; job.extranonce2_size];
    let seed = thread_index as u64;
    for i in 0..extranonce2.len() {
        extranonce2[i] = ((seed >> ((i % 8) * 8)) as u8).wrapping_add(i as u8);
    }
    let extranonce2_hex = hex::encode(&extranonce2);

    let merkle_root = build_merkle_root(&job, &extranonce2_hex);
    let header_prefix = build_header_prefix(&job, &merkle_root);
    let mut header = [0u8; 80];
    header[0..76].copy_from_slice(&header_prefix);

    let mut nonce = start_nonce;
    let mut local_count: u64 = 0;

    while stop.load(Ordering::Relaxed) {
        header[76..80].copy_from_slice(&nonce.to_le_bytes());

        let h1 = Sha256::digest(&header);
        let h2 = Sha256::digest(&h1);

        let mut display = h2.to_vec();
        display.reverse();

        if display.as_slice() <= &job.target[..] {
            let nonce_hex = format!("{:08x}", nonce);
            let _ = event_tx.send(PoolEvent::Log(format!(
                "🌟 命中! nonce={} extranonce2={}",
                nonce_hex, extranonce2_hex
            )));

            let submit_id = submit_id_counter.fetch_add(1, Ordering::SeqCst);
            let msg = json!({
                "id": submit_id,
                "method": "mining.submit",
                "params": [
                    format!("{}.{}", wallet, worker),
                    job.job_id,
                    extranonce2_hex,
                    job.ntime,
                    nonce_hex
                ]
            });

            if let Ok(mut w) = write_stream.lock() {
                let s = msg.to_string();
                let _ = w.write_all(s.as_bytes());
                let _ = w.write_all(b"\n");
                let _ = w.flush();
            }
        }

        nonce = nonce.wrapping_add(1);
        local_count += 1;

        if local_count >= 100_000 {
            total_hashes.fetch_add(local_count, Ordering::Relaxed);
            local_count = 0;
        }
    }

    total_hashes.fetch_add(local_count, Ordering::Relaxed);
}

// ─────────────────────────────────────────────────────────────────
//  SHA-256d 與 header 組裝
// ─────────────────────────────────────────────────────────────────

fn sha256d(data: &[u8]) -> [u8; 32] {
    let h1 = Sha256::digest(data);
    let h2 = Sha256::digest(&h1);
    let mut out = [0u8; 32];
    out.copy_from_slice(&h2);
    out
}

fn build_merkle_root(job: &JobInfo, extranonce2_hex: &str) -> [u8; 32] {
    let coinbase_hex = format!(
        "{}{}{}{}",
        job.coinb1, job.extranonce1, extranonce2_hex, job.coinb2
    );
    let coinbase = hex::decode(&coinbase_hex).unwrap_or_default();

    let mut root = sha256d(&coinbase);

    for branch_hex in &job.merkle_branches {
        let branch = match hex::decode(branch_hex) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let mut combined = Vec::with_capacity(64);
        combined.extend_from_slice(&root);
        combined.extend_from_slice(&branch);
        root = sha256d(&combined);
    }
    root
}

fn build_header_prefix(job: &JobInfo, merkle_root: &[u8; 32]) -> [u8; 76] {
    let mut header = [0u8; 76];

    // version (4 bytes, 反轉)
    let mut version = hex::decode(&job.version).unwrap_or_default();
    version.resize(4, 0);
    version.reverse();
    header[0..4].copy_from_slice(&version);

    // prevhash: 每 4-byte 字組單獨反轉
    let mut prevhash = hex::decode(&job.prevhash).unwrap_or_default();
    if prevhash.len() == 32 {
        for i in (0..32).step_by(4) {
            prevhash[i..i + 4].reverse();
        }
    }
    if prevhash.len() >= 32 {
        header[4..36].copy_from_slice(&prevhash[0..32]);
    }

    // merkle root（內部位元組序）
    header[36..68].copy_from_slice(merkle_root);

    // ntime (4 bytes, 反轉)
    let mut ntime = hex::decode(&job.ntime).unwrap_or_default();
    ntime.resize(4, 0);
    ntime.reverse();
    header[68..72].copy_from_slice(&ntime);

    // nbits (4 bytes, 反轉)
    let mut nbits = hex::decode(&job.nbits).unwrap_or_default();
    nbits.resize(4, 0);
    nbits.reverse();
    header[72..76].copy_from_slice(&nbits);

    header
}

// ─────────────────────────────────────────────────────────────────
//  難度 → Target（大端序 32 bytes）
// ─────────────────────────────────────────────────────────────────

fn target_from_difficulty(difficulty: f64) -> [u8; 32] {
    use num_bigint::BigUint;

    let diff1: [u8; 32] = [
        0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];
    let diff1_int = BigUint::from_bytes_be(&diff1);

    let d = if difficulty < 1e-9 { 1e-9 } else { difficulty };
    let scale: u64 = 1_000_000_000;
    let d_scaled = (d * scale as f64) as u64;
    if d_scaled == 0 {
        return [0xFFu8; 32];
    }

    let target = diff1_int * BigUint::from(scale) / BigUint::from(d_scaled);
    let target_bytes = target.to_bytes_be();

    let mut out = [0u8; 32];
    if target_bytes.len() <= 32 {
        out[32 - target_bytes.len()..].copy_from_slice(&target_bytes);
    } else {
        for b in out.iter_mut() {
            *b = 0xFF;
        }
    }
    out
}