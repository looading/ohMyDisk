//! Native desktop frontend. Filesystem work belongs to the shared local service;
//! the UI only owns navigation, an uncommitted collection, and explicit approval.
mod sunburst;

use std::{collections::HashSet, path::PathBuf, time::Duration};

use iced::widget::{
    Column, Row, Space, button, canvas, column, container, mouse_area, row, rule, scrollable,
    stack, text, text_input,
};
use iced::{
    Alignment, Background, Border, Color, Element, Font, Length, Subscription, Task, Theme,
};
use ohmydisk_core::model::{
    CleanupMode, ItemResult, ItemStatus, Job, Node, NodeKind, Volume, new_id,
};
use ohmydisk_service::ipc::Client;
use serde::Deserialize;
use serde_json::{Value, json};

const BG: Color = Color::from_rgb(0.055, 0.063, 0.09);
const PANEL: Color = Color::from_rgb(0.075, 0.086, 0.12);
const CARD: Color = Color::from_rgb(0.105, 0.119, 0.165);
const MUTED: Color = Color::from_rgb(0.57, 0.61, 0.72);
const WHITE: Color = Color::from_rgb(0.90, 0.92, 0.98);
const ACCENT: Color = Color::from_rgb(0.48, 0.50, 1.0);
const GREEN: Color = Color::from_rgb(0.39, 0.79, 0.67);
const WARNING: Color = Color::from_rgb(0.96, 0.69, 0.39);

pub fn run(data_dir: PathBuf) -> iced::Result {
    let mut app = iced::application(move || App::new(data_dir.clone()), App::update, App::view)
        .title(concat!(
            "ohMyDisk v",
            env!("CARGO_PKG_VERSION"),
            " · 空间，清晰可见"
        ))
        .subscription(App::subscription)
        .theme(Theme::Dark)
        .default_font(chinese_font())
        .window(iced::window::Settings {
            size: iced::Size::new(1370.0, 900.0),
            min_size: Some(iced::Size::new(1120.0, 760.0)),
            ..iced::window::Settings::default()
        })
        .centered();
    // iced's bundled Latin font does not contain CJK. Register an OS supplied
    // face so both Chinese labels and non-Latin filenames render on first boot.
    for path in font_candidates() {
        if let Ok(bytes) = std::fs::read(path) {
            app = app.font(bytes);
            break;
        }
    }
    app.run()
}

fn chinese_font() -> Font {
    #[cfg(target_os = "macos")]
    {
        Font::with_name("Heiti SC")
    }
    #[cfg(target_os = "windows")]
    {
        Font::with_name("Microsoft YaHei")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Font::with_name("Noto Sans CJK SC")
    }
}
fn font_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![
            "/System/Library/Fonts/STHeiti Light.ttc".into(),
            "/System/Library/Fonts/PingFang.ttc".into(),
        ]
    }
    #[cfg(target_os = "windows")]
    {
        vec![
            PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into()))
                .join("Fonts/msyh.ttc"),
        ]
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        vec!["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into()]
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ScanSummary {
    scan_id: String,
    root_id: Option<String>,
    root_path: ohmydisk_core::model::NativePath,
    #[serde(default)]
    logical_bytes: u64,
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    known_stale: bool,
    refresh_job_id: Option<String>,
    job_id: Option<String>,
    state: Option<String>,
}

#[derive(Clone, Debug)]
struct Location {
    id: String,
    name: String,
    path: String,
    bytes: u64,
}

#[derive(Clone, Debug)]
enum Response {
    Sources,
    Scans,
    StartedScan,
    Poll { id: String },
    Scan { epoch: u64 },
    Children { epoch: u64, append: bool },
    Descendants { epoch: u64 },
    PlanStarted(CleanupMode),
    Plan { append: bool },
    Executed,
    CleanupResult { append: bool },
    Cancel,
    Reveal,
    Release(String),
}

#[derive(Clone, Debug)]
enum Work {
    Scan,
    Prepare(CleanupMode),
    Cleanup,
    Refresh,
}
#[derive(Clone, Debug)]
struct ActiveJob {
    id: String,
    work: Work,
    state: String,
    progress: String,
}

#[derive(Clone, Debug)]
struct ReviewPlan {
    id: String,
    hash: String,
    mode: CleanupMode,
    bytes: u64,
    expires_at: u64,
    targets: Vec<Node>,
    manifest: Vec<Value>,
    next_cursor: Option<String>,
}

#[derive(Clone)]
enum Message {
    Connected(Result<Client, String>),
    Response(Response, Result<Value, String>),
    PickFolder,
    FolderPicked(Option<PathBuf>),
    StartScan(PathBuf),
    OpenScan(String),
    Navigate(String),
    Back,
    Forward,
    Up,
    Rescan,
    MoreChildren,
    Hover(Option<String>),
    DragStarted(Option<String>),
    ChartReleased {
        segment: Option<sunburst::Segment>,
        outside: bool,
    },
    CollectorHover(bool),
    Collect(String),
    Remove(String),
    ClearCollection,
    Prepare(CleanupMode),
    DismissReview,
    ConfirmText(String),
    Execute,
    MorePlan,
    MoreResults,
    DismissResults,
    Reveal(String),
    CancelJob,
    Tick,
    ClearBanner,
    Retry,
    CloseScan(String),
}
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GUI event")
    }
}

struct App {
    client: Option<Client>,
    data_dir: PathBuf,
    connecting: bool,
    sources: Vec<Volume>,
    scans: Vec<ScanSummary>,
    scan: Option<ScanSummary>,
    location: Option<Location>,
    breadcrumbs: Vec<Location>,
    history: Vec<Location>,
    history_index: usize,
    children: Vec<Node>,
    chart_nodes: Vec<Node>,
    segments: Vec<sunburst::Segment>,
    next_cursor: Option<String>,
    loading: bool,
    epoch: u64,
    hover: Option<String>,
    drag: Option<String>,
    collector_hover: bool,
    collection: Vec<Node>,
    draft_scan_id: Option<String>,
    active: Option<ActiveJob>,
    polling: bool,
    review: Option<ReviewPlan>,
    confirm_text: String,
    busy: bool,
    result: Option<Value>,
    result_items: Vec<ItemResult>,
    result_cursor: Option<String>,
    result_job: Option<String>,
    banner: Option<String>,
}

impl App {
    fn new(data_dir: PathBuf) -> (Self, Task<Message>) {
        let task = Self::connect(data_dir.clone());
        (
            Self {
                client: None,
                data_dir,
                connecting: true,
                sources: Vec::new(),
                scans: Vec::new(),
                scan: None,
                location: None,
                breadcrumbs: Vec::new(),
                history: Vec::new(),
                history_index: 0,
                children: Vec::new(),
                chart_nodes: Vec::new(),
                segments: Vec::new(),
                next_cursor: None,
                loading: false,
                epoch: 0,
                hover: None,
                drag: None,
                collector_hover: false,
                collection: Vec::new(),
                draft_scan_id: None,
                active: None,
                polling: false,
                review: None,
                confirm_text: String::new(),
                busy: false,
                result: None,
                result_items: Vec::new(),
                result_cursor: None,
                result_job: None,
                banner: None,
            },
            task,
        )
    }
    fn connect(path: PathBuf) -> Task<Message> {
        Task::perform(
            async move {
                Client::connect_or_spawn(path)
                    .await
                    .map_err(|e| e.to_string())
            },
            Message::Connected,
        )
    }
    fn request(&self, operation: &'static str, args: Value, response: Response) -> Task<Message> {
        let Some(client) = self.client.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                client
                    .call(operation, args)
                    .await
                    .map_err(|e| e.to_string())
            },
            move |result| Message::Response(response.clone(), result),
        )
    }
    fn reload(&self) -> Task<Message> {
        Task::batch([
            self.request("list_sources", json!({}), Response::Sources),
            self.request("list_scans", json!({"limit": 100}), Response::Scans),
        ])
    }
    fn subscription(&self) -> Subscription<Message> {
        iced::time::every(Duration::from_millis(650)).map(|_| Message::Tick)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Connected(result) => {
                self.connecting = false;
                match result {
                    Ok(client) => {
                        self.client = Some(client);
                        return self.reload();
                    }
                    Err(error) => self.banner = Some(error),
                }
            }
            Message::Retry => {
                self.connecting = true;
                self.banner = None;
                return Self::connect(self.data_dir.clone());
            }
            Message::Response(response, result) => {
                if let Response::Poll { .. } = &response {
                    self.polling = false;
                }
                match result {
                    Ok(value) => return self.receive(response, value),
                    Err(error) => {
                        self.banner = Some(error);
                        self.loading = false;
                        self.busy = false;
                        if matches!(response, Response::PlanStarted(_) | Response::Executed) {
                            self.active = None;
                        }
                    }
                }
            }
            Message::PickFolder => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("选择要分析的文件夹")
                            .pick_folder()
                            .await
                            .map(|file| file.path().to_owned())
                    },
                    Message::FolderPicked,
                );
            }
            Message::FolderPicked(Some(path)) | Message::StartScan(path) => {
                if self.active.is_some() {
                    self.banner = Some("请等待当前作业结束，或先请求停止。".into());
                } else {
                    self.busy = true;
                    return self.request("start_scan", json!({"root_path": ohmydisk_core::model::NativePath::new(&path), "request_key": new_id()}), Response::StartedScan);
                }
            }
            Message::FolderPicked(None) => {}
            Message::OpenScan(scan_id) => return self.open_scan(scan_id),
            Message::Navigate(id) => return self.navigate(&id, true),
            Message::Back if self.history_index > 0 => {
                self.history_index -= 1;
                return self.show_location(self.history[self.history_index].clone());
            }
            Message::Forward if self.history_index + 1 < self.history.len() => {
                self.history_index += 1;
                return self.show_location(self.history[self.history_index].clone());
            }
            Message::Up => {
                if self.breadcrumbs.len() > 1 {
                    return self.navigate(
                        &self.breadcrumbs[self.breadcrumbs.len() - 2].id.clone(),
                        true,
                    );
                }
            }
            Message::Rescan => {
                if let Some(scan) = &self.scan {
                    if let Ok(path) = scan.root_path.to_path() {
                        return self.update(Message::StartScan(path));
                    }
                }
            }
            Message::MoreChildren => {
                if !self.loading {
                    if let (Some(scan), Some(location), Some(cursor)) =
                        (&self.scan, &self.location, &self.next_cursor)
                    {
                        self.loading = true;
                        return self.request("list_children", json!({"scan_id":scan.scan_id,"node_id":location.id,"limit":100,"cursor":cursor}), Response::Children { epoch: self.epoch, append: true });
                    }
                }
            }
            Message::Hover(id) => self.hover = id,
            Message::DragStarted(id) => self.drag = id,
            Message::CollectorHover(hover) => self.collector_hover = hover,
            Message::ChartReleased { segment, outside } => {
                if outside && self.collector_hover {
                    if let Some(id) = self.drag.take() {
                        self.collect(&id);
                    }
                } else if let Some(segment) = segment {
                    self.drag = None;
                    if let Some(id) = segment.node_id {
                        return self.navigate(&id, true);
                    }
                    if self
                        .location
                        .as_ref()
                        .is_some_and(|location| location.id == segment.owner_id)
                    {
                        self.banner = Some(format!(
                            "灰色扇区聚合了 {} 的较小项目或尚未载入的成员。右侧列表可分页查看，聚合扇区不能加入清理。",
                            bytes(segment.bytes)
                        ));
                    } else {
                        return self.navigate(&segment.owner_id, true);
                    }
                } else {
                    self.drag = None;
                }
            }
            Message::Collect(id) => self.collect(&id),
            Message::Remove(id)
                if !self
                    .active
                    .as_ref()
                    .is_some_and(|job| matches!(job.work, Work::Cleanup)) =>
            {
                self.collection.retain(|node| node.id != id);
                if self.collection.is_empty() {
                    self.draft_scan_id = None;
                }
            }
            Message::ClearCollection
                if !self
                    .active
                    .as_ref()
                    .is_some_and(|job| matches!(job.work, Work::Cleanup)) =>
            {
                self.collection.clear();
                self.draft_scan_id = None;
            }
            Message::Prepare(mode) => {
                if !self.collection.is_empty() && self.active.is_none() && !self.busy {
                    self.busy = true;
                    return self.request("create_cleanup_plan", json!({"scan_id": self.draft_scan_id,"node_ids":self.collection.iter().map(|n| &n.id).collect::<Vec<_>>(),"mode":mode,"request_key":new_id()}), Response::PlanStarted(mode));
                }
            }
            Message::DismissReview => {
                if !self.busy {
                    self.review = None;
                    self.confirm_text.clear();
                }
            }
            Message::ConfirmText(value) => self.confirm_text = value,
            Message::Execute => {
                if let Some(plan) = &self.review {
                    if !self.busy
                        && (plan.mode == CleanupMode::Trash || self.confirm_text == "永久删除")
                    {
                        self.busy = true;
                        let operation = if plan.mode == CleanupMode::Trash {
                            "execute_trash"
                        } else {
                            "execute_permanent"
                        };
                        return self.request(
                            operation,
                            json!({"plan_id":plan.id,"plan_hash":plan.hash,"request_key":new_id()}),
                            Response::Executed,
                        );
                    }
                }
            }
            Message::MorePlan => {
                if let Some(plan) = &self.review {
                    if let Some(cursor) = &plan.next_cursor {
                        return self.request(
                            "get_cleanup_plan",
                            json!({"plan_id":plan.id,"limit":100,"cursor":cursor}),
                            Response::Plan { append: true },
                        );
                    }
                }
            }
            Message::MoreResults => {
                if let (Some(job), Some(cursor)) = (&self.result_job, &self.result_cursor) {
                    return self.request(
                        "get_cleanup_result",
                        json!({"job_id":job,"limit":100,"cursor":cursor}),
                        Response::CleanupResult { append: true },
                    );
                }
            }
            Message::DismissResults => {
                self.result = None;
                self.result_items.clear();
            }
            Message::Reveal(id) => {
                if let Some(scan) = &self.scan {
                    return self.request(
                        "reveal_node",
                        json!({"scan_id":scan.scan_id,"node_id":id}),
                        Response::Reveal,
                    );
                }
            }
            Message::CancelJob => {
                if let Some(job) = &mut self.active {
                    job.state = "cancel_requested".into();
                    let id = job.id.clone();
                    return self.request("cancel_job", json!({"job_id": id}), Response::Cancel);
                }
            }
            Message::Tick => {
                if !self.polling {
                    if let Some(job) = &self.active {
                        self.polling = true;
                        return self.request(
                            "get_job",
                            json!({"job_id":job.id}),
                            Response::Poll { id: job.id.clone() },
                        );
                    }
                }
            }
            Message::CloseScan(id) => {
                if self.draft_scan_id.as_ref() == Some(&id) && !self.collection.is_empty() {
                    self.banner = Some(
                        "这个扫描仍有待处理的收集项目。请先处理或清空收集区，再关闭记录。".into(),
                    );
                } else {
                    return self.request(
                        "release_scan",
                        json!({"scan_id":id}),
                        Response::Release(id),
                    );
                }
            }
            Message::ClearBanner => self.banner = None,
            _ => {}
        }
        Task::none()
    }

    fn receive(&mut self, response: Response, value: Value) -> Task<Message> {
        match response {
            Response::Sources => self.sources = decode_items(&value),
            Response::Scans => self.scans = decode_items(&value),
            Response::StartedScan => {
                self.busy = false;
                if let Some(id) = string(&value, "job_id") {
                    self.active = Some(ActiveJob {
                        id,
                        work: Work::Scan,
                        state: "queued".into(),
                        progress: "等待扫描…".into(),
                    });
                }
                return self.reload();
            }
            Response::Poll { id } => {
                if self.active.as_ref().is_none_or(|job| job.id != id) {
                    return Task::none();
                }
                match serde_json::from_value::<Job>(value) {
                    Ok(job) => {
                        let terminal = matches!(
                            job.state.as_str(),
                            "completed"
                                | "completed_with_errors"
                                | "cancelled"
                                | "failed"
                                | "interrupted"
                        );
                        let work = self.active.as_ref().unwrap().work.clone();
                        if let Some(active) = &mut self.active {
                            active.state = job.state.clone();
                            active.progress = format!(
                                "{} · {} 个文件 · {} 个文件夹 · 逻辑大小 {} · {} 秒",
                                phase_label(&job.progress.phase),
                                job.progress.files,
                                job.progress.directories,
                                bytes(job.progress.bytes),
                                ohmydisk_core::model::now_ms().saturating_sub(job.created_at)
                                    / 1000
                            );
                        }
                        if !terminal {
                            return Task::none();
                        }
                        self.active = None;
                        self.busy = false;
                        if let Some(error) = job.error {
                            self.banner = Some(error.to_string());
                        }
                        if matches!(job.state.as_str(), "cancelled" | "interrupted") {
                            self.banner = Some(format!(
                                "作业已{}。已完成的操作不会回滚；扫描的部分结果会明确标注。",
                                if job.state == "cancelled" {
                                    "停止"
                                } else {
                                    "中断"
                                }
                            ));
                        }
                        match work {
                            Work::Scan | Work::Refresh => {
                                if let Some(scan_id) = string(&job.result, "scan_id") {
                                    return Task::batch([self.open_scan(scan_id), self.reload()]);
                                }
                            }
                            Work::Prepare(mode) => {
                                if let Some(plan_id) = string(&job.result, "plan_id") {
                                    self.confirm_text.clear();
                                    self.banner = Some(
                                        if mode == CleanupMode::Trash {
                                            "回收站计划已准备好，请核对目标后确认。"
                                        } else {
                                            "永久删除计划已准备好，请逐项核对。"
                                        }
                                        .into(),
                                    );
                                    return self.request(
                                        "get_cleanup_plan",
                                        json!({"plan_id":plan_id,"limit":100}),
                                        Response::Plan { append: false },
                                    );
                                }
                            }
                            Work::Cleanup => {
                                self.result_job = Some(id.clone());
                                return self.request(
                                    "get_cleanup_result",
                                    json!({"job_id":id,"limit":100}),
                                    Response::CleanupResult { append: false },
                                );
                            }
                        }
                        return self.reload();
                    }
                    Err(error) => self.banner = Some(format!("无法读取作业：{error}")),
                }
            }
            Response::Scan { epoch } if epoch == self.epoch => {
                match serde_json::from_value::<ScanSummary>(value) {
                    Ok(scan) => {
                        let Some(root) = scan.root_id.clone() else {
                            self.loading = false;
                            let status = scan_size_summary(&scan);
                            if let Some(id) = scan.job_id {
                                if scan.state.as_deref().is_some_and(|state| {
                                    matches!(state, "queued" | "running" | "cancel_requested")
                                }) && self.active.is_none()
                                {
                                    self.active = Some(ActiveJob {
                                        id,
                                        work: Work::Scan,
                                        state: scan.state.unwrap_or_else(|| "running".into()),
                                        progress: "已连接到后台扫描，正在读取进度…".into(),
                                    });
                                    self.banner = Some(
                                        "已接续这个后台扫描，可在此查看进度或请求停止。".into(),
                                    );
                                } else {
                                    self.banner = Some(status);
                                }
                            }
                            return Task::none();
                        };
                        let location = Location {
                            id: root,
                            name: path_name(&scan.root_path.display),
                            path: scan.root_path.display.clone(),
                            bytes: scan.logical_bytes,
                        };
                        if scan.known_stale && self.active.is_none() {
                            if let Some(id) = &scan.refresh_job_id {
                                self.active = Some(ActiveJob {
                                    id: id.clone(),
                                    work: Work::Refresh,
                                    state: "queued".into(),
                                    progress: "正在取得后台刷新状态…".into(),
                                });
                            }
                        }
                        self.history = vec![location.clone()];
                        self.history_index = 0;
                        self.scan = Some(scan);
                        return self.show_location(location);
                    }
                    Err(error) => {
                        self.loading = false;
                        self.banner = Some(format!("无法读取扫描摘要：{error}"));
                    }
                }
            }
            Response::Children { epoch, append } if epoch == self.epoch => {
                self.loading = false;
                let nodes: Vec<Node> = decode_items(&value);
                if append && self.children.len() + nodes.len() <= 500 {
                    self.children.extend(nodes);
                } else {
                    self.children = nodes;
                }
                self.next_cursor = string(&value, "next_cursor");
                self.rebuild_chart();
            }
            Response::Descendants { epoch } if epoch == self.epoch => {
                self.chart_nodes = decode_items(&value);
                self.rebuild_chart();
            }
            Response::PlanStarted(mode) => {
                self.busy = false;
                if let Some(id) = string(&value, "job_id") {
                    self.active = Some(ActiveJob {
                        id,
                        work: Work::Prepare(mode),
                        state: "queued".into(),
                        progress: "正在核对文件身份和完整目录清单…".into(),
                    });
                }
            }
            Response::Plan { append } => {
                let targets = value
                    .get("targets")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                serde_json::from_value(item.get("node").unwrap_or(item).clone())
                                    .ok()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let manifest = value
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if append {
                    if let Some(plan) = &mut self.review {
                        plan.manifest.extend(manifest);
                        plan.next_cursor = string(&value, "next_cursor");
                    }
                } else if let (Some(id), Some(hash)) = (
                    string(&value, "id").or_else(|| string(&value, "plan_id")),
                    string(&value, "hash").or_else(|| string(&value, "plan_hash")),
                ) {
                    self.review = Some(ReviewPlan {
                        id,
                        hash,
                        mode: serde_json::from_value(
                            value.get("mode").cloned().unwrap_or(json!("trash")),
                        )
                        .unwrap_or(CleanupMode::Trash),
                        bytes: value["logical_bytes"].as_u64().unwrap_or(0),
                        expires_at: value["expires_at"].as_u64().unwrap_or(0),
                        targets,
                        manifest,
                        next_cursor: string(&value, "next_cursor"),
                    });
                } else {
                    self.banner = Some("服务返回的计划缺少标识或校验值，未开放执行。".into());
                }
            }
            Response::Executed => {
                self.busy = false;
                self.review = None;
                self.confirm_text.clear();
                if let Some(id) = string(&value, "job_id") {
                    self.active = Some(ActiveJob {
                        id,
                        work: Work::Cleanup,
                        state: "queued".into(),
                        progress: "正在逐项处理已确认的清理计划…".into(),
                    });
                }
            }
            Response::CleanupResult { append } => {
                let items: Vec<ItemResult> = decode_items(&value);
                if append {
                    self.result_items.extend(items);
                } else {
                    self.result_items = items;
                }
                self.result_cursor = string(&value, "next_cursor");
                self.result = Some(value.clone());
                if !append {
                    // Only the execution receipt clears the draft; selecting,
                    // preparing, and approving a plan never alters a snapshot.
                    self.collection.clear();
                    self.draft_scan_id = None;
                    if let Some(scan) = &mut self.scan {
                        scan.known_stale = true;
                    }
                    if let Some(job) = string(&value, "refresh_job_id") {
                        self.active = Some(ActiveJob {
                            id: job,
                            work: Work::Refresh,
                            state: "queued".into(),
                            progress: "清理结果已保存，正在重新扫描原位置…".into(),
                        });
                    } else if let Some(id) = string(&value, "refresh_scan_id") {
                        return Task::batch([self.open_scan(id), self.reload()]);
                    }
                    return self.reload();
                }
            }
            Response::Release(id) => {
                if self.scan.as_ref().is_some_and(|scan| scan.scan_id == id) {
                    self.scan = None;
                    self.location = None;
                    self.breadcrumbs.clear();
                    self.history.clear();
                    self.children.clear();
                    self.chart_nodes.clear();
                    self.segments.clear();
                }
                self.banner = Some("已关闭扫描记录。源文件未被改动。".into());
                return self.reload();
            }
            _ => {}
        }
        Task::none()
    }

    fn open_scan(&mut self, id: String) -> Task<Message> {
        self.epoch += 1;
        self.loading = true;
        self.hover = None;
        self.request(
            "get_scan",
            json!({"scan_id": id}),
            Response::Scan { epoch: self.epoch },
        )
    }
    fn find_node(&self, id: &str) -> Option<&Node> {
        self.children
            .iter()
            .chain(self.chart_nodes.iter())
            .chain(self.collection.iter())
            .find(|node| node.id == id)
    }
    fn navigate(&mut self, id: &str, record: bool) -> Task<Message> {
        let location = if let Some(node) = self.find_node(id) {
            if node.kind != NodeKind::Directory {
                self.hover = Some(id.to_owned());
                return Task::none();
            }
            Location {
                id: node.id.clone(),
                name: node.name.clone(),
                path: node.path.display.clone(),
                bytes: node.logical_bytes,
            }
        } else if let Some(location) = self.breadcrumbs.iter().find(|location| location.id == id) {
            location.clone()
        } else if let Some(location) = self.history.iter().find(|location| location.id == id) {
            location.clone()
        } else {
            return Task::none();
        };
        if record
            && self
                .location
                .as_ref()
                .is_none_or(|current| current.id != id)
        {
            self.history.truncate(self.history_index + 1);
            self.history.push(location.clone());
            self.history_index = self.history.len() - 1;
        }
        self.show_location(location)
    }
    fn show_location(&mut self, location: Location) -> Task<Message> {
        let Some(scan) = &self.scan else {
            return Task::none();
        };
        let scan_id = scan.scan_id.clone();
        if let Some(index) = self
            .breadcrumbs
            .iter()
            .position(|entry| entry.id == location.id)
        {
            self.breadcrumbs.truncate(index + 1);
        } else {
            let mut chain = vec![location.clone()];
            let mut parent = self
                .find_node(&location.id)
                .and_then(|node| node.parent_id.clone());
            let mut seen = HashSet::new();
            while let Some(id) = parent {
                if !seen.insert(id.clone()) {
                    break;
                }
                if let Some(ancestor) = self.breadcrumbs.iter().find(|entry| entry.id == id) {
                    chain.push(ancestor.clone());
                    parent = None;
                } else if let Some(node) = self.find_node(&id) {
                    chain.push(Location {
                        id: node.id.clone(),
                        name: node.name.clone(),
                        path: node.path.display.clone(),
                        bytes: node.logical_bytes,
                    });
                    parent = node.parent_id.clone();
                } else {
                    break;
                }
            }
            chain.reverse();
            if let Some(root) = self.history.first() {
                if chain.first().is_none_or(|entry| entry.id != root.id) {
                    chain.insert(0, root.clone());
                }
            }
            self.breadcrumbs = chain;
        }
        self.location = Some(location.clone());
        self.children.clear();
        self.chart_nodes.clear();
        self.segments.clear();
        self.next_cursor = None;
        self.hover = None;
        self.loading = true;
        self.epoch += 1;
        let epoch = self.epoch;
        Task::batch([
            self.request("list_children", json!({"scan_id":scan_id,"node_id":location.id,"limit":100}), Response::Children { epoch, append:false }),
            self.request("query_nodes", json!({"scan_id":scan_id,"scope_node_id":location.id,"descendants":true,"limit":500}), Response::Descendants { epoch }),
        ])
    }
    fn rebuild_chart(&mut self) {
        let Some(location) = &self.location else {
            return;
        };
        let mut known = HashSet::new();
        let mut nodes: Vec<Node> = self
            .children
            .iter()
            .chain(self.chart_nodes.iter())
            .filter(|node| known.insert(node.id.clone()))
            .take(500)
            .cloned()
            .collect();
        nodes.sort_by(|a, b| b.logical_bytes.cmp(&a.logical_bytes));
        self.segments = sunburst::layout(&location.id, location.bytes, &nodes);
    }
    fn collect(&mut self, id: &str) {
        if self
            .active
            .as_ref()
            .is_some_and(|job| matches!(job.work, Work::Cleanup))
            || self.busy
        {
            self.banner = Some("清理作业进行中，请等待逐项结果后再修改收集区。".into());
            return;
        }
        let Some(scan) = &self.scan else {
            return;
        };
        if self
            .draft_scan_id
            .as_ref()
            .is_some_and(|draft| draft != &scan.scan_id)
        {
            self.banner =
                Some("收集区来自另一份扫描。请先处理或清空，再加入当前扫描的项目。".into());
            return;
        }
        let Some(node) = self.find_node(id).cloned() else {
            return;
        };
        if Some(node.id.as_str()) == scan.root_id.as_deref() {
            self.banner = Some("扫描根不能作为清理目标，请选择里面的文件或文件夹。".into());
            return;
        }
        if !node.complete {
            self.banner = Some("该项目的扫描不完整，请先处理访问问题并重新扫描。".into());
            return;
        }
        if self
            .collection
            .iter()
            .any(|entry| node_covers(entry, &node))
        {
            return;
        }
        self.collection.retain(|entry| !node_covers(&node, entry));
        if self.collection.len() >= 500 {
            self.banner = Some("单个清理计划最多选择 500 个目标；可选择其父目录。".into());
            return;
        }
        self.draft_scan_id = Some(scan.scan_id.clone());
        self.collection.push(node);
    }

    fn view(&self) -> Element<'_, Message> {
        let header = row![
            row![
                text("◉").size(29).color(ACCENT),
                text("ohMyDisk").size(24),
                text("空间，清晰可见").size(12).color(MUTED)
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            Space::new().width(Length::Fill),
            text(if self.connecting {
                "正在连接本地服务…"
            } else if self.client.is_some() {
                "本地分析 · 文件留在你的设备"
            } else {
                "本地服务未连接"
            })
            .size(12)
            .color(MUTED),
            button(text("＋ 选择文件夹").size(13))
                .padding([10, 16])
                .style(accent_button)
                .on_press_maybe(
                    self.client
                        .as_ref()
                        .filter(|_| self.active.is_none())
                        .map(|_| Message::PickFolder)
                ),
        ]
        .spacing(22)
        .padding([18, 24])
        .align_y(Alignment::Center);
        let mut content = column![header, rule::horizontal(1)].spacing(0);
        if let Some(banner) = &self.banner {
            content = content.push(
                container(
                    row![
                        text(banner).size(12).color(WARNING),
                        Space::new().width(Length::Fill),
                        button("×")
                            .style(quiet_button)
                            .on_press(Message::ClearBanner)
                    ]
                    .align_y(Alignment::Center)
                    .spacing(12),
                )
                .padding([8, 20])
                .width(Length::Fill)
                .style(|_| panel_style(Color::from_rgb(0.17, 0.13, 0.105))),
            );
        }
        let workspace = row![
            self.sidebar(),
            column![self.navigation(), self.analysis()]
                .spacing(0)
                .width(Length::Fill)
        ]
        .spacing(0)
        .height(Length::Fill);
        content = content.push(workspace).push(self.collector());
        let main: Element<'_, Message> = container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| panel_style(BG))
            .into();
        if let Some(plan) = &self.review {
            stack![
                main,
                iced::widget::opaque(
                    container(self.review_view(plan))
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
                        .style(|_| panel_style(Color::from_rgba(0.015, 0.02, 0.035, 0.88)))
                )
            ]
            .into()
        } else if self.result.is_some() {
            stack![
                main,
                iced::widget::opaque(
                    container(self.result_view())
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .center_x(Length::Fill)
                        .center_y(Length::Fill)
                        .style(|_| panel_style(Color::from_rgba(0.015, 0.02, 0.035, 0.88)))
                )
            ]
            .into()
        } else {
            main
        }
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let mut items = column![text("扫描对象").size(11).color(MUTED)].spacing(12);
        for source in &self.sources {
            let used = source.total_bytes.saturating_sub(source.available_bytes);
            let ratio = if source.total_bytes > 0 {
                used as f32 / source.total_bytes as f32
            } else {
                0.0
            };
            let mut source_content = column![
                row![
                    text("▣").size(18).color(ACCENT),
                    text(crop(&source.name, 22)).size(14),
                    Space::new().width(Length::Fill)
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                text(format!(
                    "{} 可用 / {}",
                    bytes(source.available_bytes),
                    bytes(source.total_bytes)
                ))
                .size(11)
                .color(MUTED),
                iced::widget::progress_bar(0.0..=1.0, ratio).girth(4),
                text(crop(&source.mount_path.display, 28))
                    .size(10)
                    .color(MUTED),
            ]
            .spacing(10);
            if let Some(hint) = source_hint(source) {
                source_content = source_content.push(text(hint).size(10).color(MUTED));
            }
            let path = source.mount_path.to_path().ok();
            items = items.push(
                button(source_content)
                    .width(Length::Fill)
                    .padding(14)
                    .style(card_button)
                    .on_press_maybe(
                        path.filter(|_| self.active.is_none())
                            .map(Message::StartScan),
                    ),
            );
        }
        if self.sources.is_empty() {
            items = items.push(
                text(if self.connecting {
                    "正在发现磁盘…"
                } else {
                    "选择文件夹开始分析"
                })
                .size(12)
                .color(MUTED),
            );
        }
        if cfg!(target_os = "macos")
            && self
                .sources
                .iter()
                .any(|source| source.mount_path.display == "/")
            && self
                .sources
                .iter()
                .any(|source| source.mount_path.display == "/System/Volumes/Data")
        {
            items = items.push(
                text("系统与数据入口共享 APFS 容器空间，容量请勿相加。")
                    .size(10)
                    .color(MUTED),
            );
        }
        items = items.push(Space::new().height(14)).push(row![
            text("已保存的扫描").size(11).color(MUTED),
            Space::new().width(Length::Fill),
            text(self.scans.len().to_string()).size(11).color(MUTED)
        ]);
        for scan in &self.scans {
            let selected = self
                .scan
                .as_ref()
                .is_some_and(|current| current.scan_id == scan.scan_id);
            let name = path_name(&scan.root_path.display);
            let info = column![
                text(crop(&name, 20))
                    .size(13)
                    .color(if selected { WHITE } else { MUTED }),
                text(scan_size_summary(scan)).size(10).color(MUTED),
            ]
            .spacing(6);
            items = items.push(
                row![
                    button(info)
                        .padding([12, 10])
                        .width(Length::Fill)
                        .style(move |theme, status| if selected {
                            selected_button(theme, status)
                        } else {
                            quiet_button(theme, status)
                        })
                        .on_press(Message::OpenScan(scan.scan_id.clone())),
                    button(text("×").size(15))
                        .padding(7)
                        .style(quiet_button)
                        .on_press(Message::CloseScan(scan.scan_id.clone()))
                ]
                .spacing(2)
                .align_y(Alignment::Center),
            );
        }
        let mut sidebar = column![
            scrollable(items).height(Length::Fill),
            rule::horizontal(1),
            text(
                "逻辑大小可超过卷容量\n不等于实际占用或可释放空间\n不跟随链接 · 重复目录不重复遍历"
            )
            .size(10)
            .color(MUTED)
        ]
        .spacing(18);
        if self.client.is_none() && !self.connecting {
            sidebar = sidebar.push(
                button("重新连接")
                    .on_press(Message::Retry)
                    .style(accent_button),
            );
        }
        container(sidebar)
            .padding([24, 18])
            .width(230)
            .height(Length::Fill)
            .style(|_| panel_style(PANEL))
            .into()
    }

    fn navigation(&self) -> Element<'_, Message> {
        let controls = row![
            button("‹")
                .padding([7, 12])
                .style(quiet_button)
                .on_press_maybe((self.history_index > 0).then_some(Message::Back)),
            button("›")
                .padding([7, 12])
                .style(quiet_button)
                .on_press_maybe(
                    (self.history_index + 1 < self.history.len()).then_some(Message::Forward)
                ),
            button("↑")
                .padding([7, 12])
                .style(quiet_button)
                .on_press_maybe((self.breadcrumbs.len() > 1).then_some(Message::Up)),
        ]
        .spacing(3);
        let mut path = Row::new().spacing(3).align_y(Alignment::Center);
        if self.breadcrumbs.is_empty() {
            path = path.push(text("选择一个位置，看看空间去了哪里").size(12).color(MUTED));
        }
        let skip = self.breadcrumbs.len().saturating_sub(4);
        for (index, location) in self.breadcrumbs.iter().enumerate().skip(skip) {
            if index > skip {
                path = path.push(text("/").size(13).color(MUTED));
            }
            path = path.push(
                button(text(crop(&location.name, 22)).size(12))
                    .padding([7, 5])
                    .style(quiet_button)
                    .on_press(Message::Navigate(location.id.clone())),
            );
        }
        container(
            row![
                controls,
                path,
                Space::new().width(Length::Fill),
                button(text("↻ 重新扫描").size(12))
                    .padding([8, 12])
                    .style(quiet_button)
                    .on_press_maybe(
                        self.scan
                            .as_ref()
                            .filter(|_| self.active.is_none())
                            .map(|_| Message::Rescan)
                    )
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        )
        .padding([14, 20])
        .width(Length::Fill)
        .style(|_| panel_style(BG))
        .into()
    }

    fn analysis(&self) -> Element<'_, Message> {
        if self.scan.is_none() {
            let mut empty = column![
                text("让每一份空间，都有迹可循。").size(30).color(WHITE),
                text("选择磁盘或文件夹，按大小探索你的文件。\n先收集，再核对；清理由你决定。")
                    .size(15)
                    .color(MUTED)
                    .line_height(1.8),
                button(text("选择文件夹开始").size(14))
                    .padding([13, 24])
                    .style(accent_button)
                    .on_press_maybe(self.client.as_ref().map(|_| Message::PickFolder)),
                text("支持隐藏项目 · 本地扫描 · 可随时请求停止")
                    .size(11)
                    .color(MUTED),
            ]
            .spacing(24)
            .align_x(Alignment::Center);
            if let Some(job) = &self.active {
                empty = empty.push(
                    container(
                        column![
                            text(job_label(&job.work)).size(16).color(ACCENT),
                            text(format!("{} · {}", state_label(&job.state), job.progress))
                                .size(12)
                                .color(MUTED),
                            button(
                                text(if job.state == "cancel_requested" {
                                    "正在停止…"
                                } else {
                                    "请求停止扫描"
                                })
                                .size(12)
                            )
                            .padding([10, 18])
                            .style(quiet_button)
                            .on_press_maybe(
                                (job.state != "cancel_requested").then_some(Message::CancelJob)
                            ),
                        ]
                        .spacing(15)
                        .align_x(Alignment::Center),
                    )
                    .padding(24)
                    .style(|_| rounded_style(CARD, 14.0)),
                );
            }
            return container(empty)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        let scan = self.scan.as_ref().unwrap();
        let focus = self.hover.as_deref().and_then(|id| self.find_node(id));
        let title = focus
            .map(|node| node.name.clone())
            .or_else(|| self.location.as_ref().map(|location| location.name.clone()))
            .unwrap_or_default();
        let amount = focus
            .map(|node| node.logical_bytes)
            .or_else(|| self.location.as_ref().map(|location| location.bytes))
            .unwrap_or(0);
        let current_path = focus
            .map(|node| node.path.display.as_str())
            .or_else(|| {
                self.location
                    .as_ref()
                    .map(|location| location.path.as_str())
            })
            .unwrap_or("");
        let collected = self
            .collection
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        let graph = canvas(sunburst::Sunburst {
            segments: &self.segments,
            hover: self.hover.as_deref(),
            collected,
        })
        .width(Length::Fill)
        .height(Length::Fill);
        let center = container(
            column![
                text("逻辑大小").size(10).color(MUTED),
                text(bytes(amount)).size(27).color(WHITE),
                text(crop(&title, 18)).size(11).color(MUTED)
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
        let mut chart = column![
            row![
                column![
                    text(crop(
                        &self
                            .location
                            .as_ref()
                            .map(|l| l.name.as_str())
                            .unwrap_or("空间分析"),
                        28
                    ))
                    .size(22),
                    text(if self.loading {
                        "正在载入目录…"
                    } else {
                        "悬停详情 · 点击下钻 · 最多 4 层 / 500 节点 · 灰色为聚合"
                    })
                    .size(11)
                    .color(MUTED)
                ]
                .spacing(8),
                Space::new().width(Length::Fill),
                pill(
                    if scan.known_stale {
                        "旧快照 · 正在刷新"
                    } else if scan.complete {
                        "扫描快照"
                    } else {
                        "扫描不完整"
                    },
                    if scan.complete && !scan.known_stale {
                        GREEN
                    } else {
                        WARNING
                    }
                )
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            stack![graph, center],
            container(
                row![
                    column![
                        text(crop(&title, 40)).size(14),
                        text(crop(current_path, 78)).size(11).color(MUTED)
                    ]
                    .spacing(7)
                    .width(Length::Fill),
                    button(text("＋ 加入收集").size(12))
                        .padding([10, 14])
                        .style(accent_button)
                        .on_press_maybe(
                            focus
                                .filter(|node| node.complete)
                                .map(|node| Message::Collect(node.id.clone()))
                        )
                ]
                .spacing(14)
                .align_y(Alignment::Center)
            )
            .padding(16)
            .width(Length::Fill)
            .style(|_| rounded_style(CARD, 12.0)),
            text(format!(
                "逻辑大小 {} · 已选 {} · 未选 {} · {}",
                bytes(scan.logical_bytes),
                bytes(self.selected_bytes_for_current_scan()),
                bytes(
                    scan.logical_bytes
                        .saturating_sub(self.selected_bytes_for_current_scan())
                ),
                timestamp(scan.created_at)
            ))
            .size(10)
            .color(MUTED),
        ]
        .spacing(16)
        .padding([12, 26])
        .width(Length::Fill)
        .height(Length::Fill);
        if let Some(job) = &self.active {
            chart = chart.push(
                container(
                    row![
                        column![
                            text(job_label(&job.work)).size(12).color(ACCENT),
                            text(format!("{} · {}", state_label(&job.state), job.progress))
                                .size(11)
                                .color(MUTED)
                        ]
                        .spacing(5)
                        .width(Length::Fill),
                        button(
                            text(if job.state == "cancel_requested" {
                                "正在停止…"
                            } else {
                                "请求停止"
                            })
                            .size(11)
                        )
                        .padding(8)
                        .style(quiet_button)
                        .on_press_maybe(
                            (job.state != "cancel_requested").then_some(Message::CancelJob)
                        )
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .padding(12)
                .style(|_| rounded_style(PANEL, 10.0)),
            );
        } else if let Some(refresh) = &scan.refresh_job_id {
            chart = chart.push(
                text(format!("刷新作业：{}", crop(refresh, 16)))
                    .size(10)
                    .color(MUTED),
            );
        }
        row![chart, self.directory_list()]
            .spacing(0)
            .height(Length::Fill)
            .into()
    }

    fn directory_list(&self) -> Element<'_, Message> {
        let mut list = Column::new().spacing(5);
        for (index, node) in self.children.iter().enumerate() {
            let hovered = self.hover.as_deref() == Some(&node.id);
            let selected = self.collection.iter().any(|entry| node_covers(entry, node));
            let color = sunburst::color(index, 0);
            let kind = match node.kind {
                NodeKind::Directory => "目录",
                NodeKind::File => "文件",
                NodeKind::Symlink => "链接",
                NodeKind::Other => "其他",
            };
            let label = column![
                row![
                    text(if node.kind == NodeKind::Directory {
                        "▸"
                    } else {
                        "·"
                    })
                    .size(15)
                    .color(color),
                    text(crop(&node.name, 24))
                        .size(12)
                        .color(if selected { MUTED } else { WHITE }),
                    Space::new().width(Length::Fill),
                    text(bytes(node.logical_bytes)).size(11).color(MUTED)
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                row![
                    text(format!(
                        "{kind}{}{}",
                        if !node.complete { " · 不完整" } else { "" },
                        if selected { " · 已收集" } else { "" }
                    ))
                    .size(10)
                    .color(MUTED),
                    Space::new().width(Length::Fill),
                    text(if node.kind == NodeKind::Directory {
                        "点击进入"
                    } else {
                        ""
                    })
                    .size(10)
                    .color(MUTED)
                ]
                .spacing(6),
            ]
            .spacing(7);
            let body = row![
                button(label)
                    .width(Length::Fill)
                    .padding(11)
                    .style(move |theme, status| if hovered {
                        selected_button(theme, status)
                    } else {
                        quiet_button(theme, status)
                    })
                    .on_press(Message::Navigate(node.id.clone())),
                button(text(if selected { "✓" } else { "＋" }).size(17))
                    .padding([10, 7])
                    .style(quiet_button)
                    .on_press_maybe(
                        (!selected && node.complete).then(|| Message::Collect(node.id.clone()))
                    )
            ]
            .spacing(2)
            .align_y(Alignment::Center);
            list = list.push(
                mouse_area(body)
                    .on_enter(Message::Hover(Some(node.id.clone())))
                    .on_exit(Message::Hover(None)),
            );
        }
        if self.children.is_empty() {
            list = list.push(
                container(
                    text(if self.loading {
                        "正在读取目录…"
                    } else {
                        "此目录没有可列出的子项"
                    })
                    .size(12)
                    .color(MUTED),
                )
                .padding(15),
            );
        }
        if self.next_cursor.is_some() {
            list = list.push(
                button(
                    text(if self.loading {
                        "载入中…"
                    } else if self.children.len() >= 500 {
                        "下一页（保留最近 500 项）"
                    } else {
                        "载入更多项目"
                    })
                    .size(12),
                )
                .width(Length::Fill)
                .padding(12)
                .style(card_button)
                .on_press_maybe((!self.loading).then_some(Message::MoreChildren)),
            );
        }
        let detail = self.hover.as_deref().and_then(|id| self.find_node(id));
        let mut pane = column![
            row![
                text("按大小排列").size(13),
                Space::new().width(Length::Fill),
                text(format!(
                    "{} 项{}",
                    self.children.len(),
                    if self.next_cursor.is_some() {
                        "＋"
                    } else {
                        ""
                    }
                ))
                .size(10)
                .color(MUTED)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            rule::horizontal(1),
            scrollable(list).height(Length::Fill)
        ]
        .spacing(14);
        if let Some(node) = detail {
            pane = pane
                .push(rule::horizontal(1))
                .push(text(crop(&node.path.display, 74)).size(10).color(MUTED))
                .push(
                    button(text("在文件管理器中显示").size(11))
                        .padding(9)
                        .style(quiet_button)
                        .on_press(Message::Reveal(node.id.clone())),
                );
        }
        container(pane)
            .padding([22, 15])
            .width(335)
            .height(Length::Fill)
            .style(|_| panel_style(PANEL))
            .into()
    }

    fn selected_bytes_for_current_scan(&self) -> u64 {
        if self.scan.as_ref().map(|scan| &scan.scan_id) == self.draft_scan_id.as_ref() {
            self.collection
                .iter()
                .map(|node| node.logical_bytes)
                .fold(0_u64, u64::saturating_add)
        } else {
            0
        }
    }

    fn collector(&self) -> Element<'_, Message> {
        let total = self
            .collection
            .iter()
            .map(|node| node.logical_bytes)
            .fold(0_u64, u64::saturating_add);
        let mut chips = Row::new().spacing(10).align_y(Alignment::Center);
        if self.collection.is_empty() {
            chips = chips.push(
                text("把图表中的项目拖到这里，或使用「＋ 加入收集」。收集只保存选择。")
                    .size(12)
                    .color(MUTED),
            );
        }
        for node in &self.collection {
            chips = chips.push(
                container(
                    row![
                        column![
                            text(crop(&node.name, 21)).size(12),
                            text(bytes(node.logical_bytes)).size(10).color(MUTED)
                        ]
                        .spacing(5),
                        button(text("×").size(15))
                            .padding(5)
                            .style(quiet_button)
                            .on_press(Message::Remove(node.id.clone()))
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                )
                .padding([10, 13])
                .style(|_| rounded_style(CARD, 10.0)),
            );
        }
        let enabled = !self.collection.is_empty() && self.active.is_none() && !self.busy;
        let title = row![
            text("清理收集区").size(15),
            pill(format!("{} 项", self.collection.len()), MUTED),
            text("快照总量不会随选择减少").size(11).color(MUTED),
            Space::new().width(Length::Fill),
            button(text("清空").size(11))
                .padding(6)
                .style(quiet_button)
                .on_press_maybe(
                    (!self.collection.is_empty() && !self.busy).then_some(Message::ClearCollection)
                )
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let actions = column![
            text(bytes(total)).size(24),
            text("选中项目的逻辑大小").size(10).color(MUTED),
            row![
                button(text("永久删除…").size(11))
                    .padding([11, 12])
                    .style(quiet_button)
                    .on_press_maybe(enabled.then_some(Message::Prepare(CleanupMode::Permanent))),
                button(
                    text(if self.busy {
                        "正在准备…"
                    } else {
                        "移入回收站…"
                    })
                    .size(12)
                )
                .padding([11, 16])
                .style(accent_button)
                .on_press_maybe(enabled.then_some(Message::Prepare(CleanupMode::Trash)))
            ]
            .spacing(6)
        ]
        .spacing(7)
        .align_x(Alignment::End);
        let body = column![
            title,
            row![
                scrollable(chips)
                    .direction(scrollable::Direction::Horizontal(
                        scrollable::Scrollbar::default()
                    ))
                    .width(Length::Fill),
                actions
            ]
            .spacing(24)
            .align_y(Alignment::Center)
        ]
        .spacing(16);
        mouse_area(
            container(body)
                .padding([18, 24])
                .width(Length::Fill)
                .style(move |_| {
                    let mut style = panel_style(if self.collector_hover && self.drag.is_some() {
                        Color::from_rgb(0.15, 0.16, 0.27)
                    } else {
                        PANEL
                    });
                    style.border = Border {
                        color: if self.collector_hover && self.drag.is_some() {
                            ACCENT
                        } else {
                            Color::from_rgb(0.15, 0.17, 0.23)
                        },
                        width: 1.0,
                        ..Border::default()
                    };
                    style
                }),
        )
        .on_enter(Message::CollectorHover(true))
        .on_exit(Message::CollectorHover(false))
        .into()
    }

    fn review_view<'a>(&'a self, plan: &'a ReviewPlan) -> Element<'a, Message> {
        let permanent = plan.mode == CleanupMode::Permanent;
        let mut targets = Column::new().spacing(9);
        for node in &plan.targets {
            targets = targets.push(
                container(
                    column![
                        row![
                            text(crop(&node.name, 64)).size(13),
                            Space::new().width(Length::Fill),
                            text(bytes(node.logical_bytes)).size(12).color(MUTED)
                        ]
                        .spacing(12),
                        text(&node.path.display).size(11).color(MUTED)
                    ]
                    .spacing(6),
                )
                .padding(12)
                .width(Length::Fill)
                .style(|_| rounded_style(CARD, 8.0)),
            );
        }
        let mut detail = column![
            text("服务端核对后的精确目标").size(12).color(MUTED),
            targets
        ]
        .spacing(10);
        if !plan.manifest.is_empty() {
            detail = detail.push(
                text(format!("目录成员清单 · 已载入 {} 项", plan.manifest.len()))
                    .size(12)
                    .color(MUTED),
            );
            for item in &plan.manifest {
                let node = item.get("node").unwrap_or(item);
                let path = node
                    .pointer("/path/display")
                    .and_then(Value::as_str)
                    .or_else(|| node.get("path").and_then(Value::as_str))
                    .or_else(|| node.get("name").and_then(Value::as_str))
                    .unwrap_or("清单成员");
                detail = detail.push(text(path).size(10).color(MUTED));
            }
        }
        if plan.next_cursor.is_some() {
            detail = detail.push(
                button(text("继续查看完整成员清单").size(12))
                    .padding(10)
                    .style(quiet_button)
                    .on_press(Message::MorePlan),
            );
        }
        let mut body = column![
            row![
                text(if permanent {
                    "确认永久删除"
                } else {
                    "确认移入回收站"
                })
                .size(24),
                Space::new().width(Length::Fill),
                button("×")
                    .style(quiet_button)
                    .on_press_maybe((!self.busy).then_some(Message::DismissReview))
            ]
            .align_y(Alignment::Center),
            text(if permanent {
                "这些项目将永久删除，无法通过回收站恢复。请核对下面的路径与操作方式。"
            } else {
                "下面的项目将移入系统回收站。回收站占用仍可能保留，实际可用容量以清理后观测为准。"
            })
            .size(13)
            .color(if permanent { WARNING } else { MUTED }),
            row![
                pill(
                    if permanent {
                        "方式：永久删除"
                    } else {
                        "方式：回收站"
                    },
                    if permanent { WARNING } else { GREEN }
                ),
                text(format!(
                    "{} 个目标 · {}",
                    plan.targets.len(),
                    bytes(plan.bytes)
                ))
                .size(13),
                Space::new().width(Length::Fill)
            ]
            .spacing(14)
            .align_y(Alignment::Center),
            scrollable(detail).height(Length::Fill),
            text(format!(
                "计划有效期至 {} · 文件发生变化时服务会跳过并报告",
                timestamp(plan.expires_at)
            ))
            .size(10)
            .color(MUTED),
        ]
        .spacing(18);
        if permanent {
            body = body.push(
                text_input("输入「永久删除」确认此操作方式", &self.confirm_text)
                    .on_input(Message::ConfirmText)
                    .padding(13)
                    .size(13),
            );
        }
        if let Some(banner) = &self.banner {
            body = body.push(text(banner).size(11).color(WARNING));
        }
        let enabled = !self.busy
            && ohmydisk_core::model::now_ms() <= plan.expires_at
            && !plan.targets.is_empty()
            && (!permanent || self.confirm_text == "永久删除");
        body = body.push(
            row![
                button(text("返回检查").size(13))
                    .padding([12, 18])
                    .style(quiet_button)
                    .on_press_maybe((!self.busy).then_some(Message::DismissReview)),
                Space::new().width(Length::Fill),
                button(
                    text(if self.busy {
                        "正在提交…"
                    } else if permanent {
                        "永久删除这些项目"
                    } else {
                        "确认移入回收站"
                    })
                    .size(13)
                )
                .padding([12, 20])
                .style(if permanent {
                    button::danger
                } else {
                    accent_button
                })
                .on_press_maybe(enabled.then_some(Message::Execute))
            ]
            .spacing(12),
        );
        container(body)
            .padding(30)
            .width(760)
            .height(680)
            .style(|_| rounded_style(PANEL, 20.0))
            .into()
    }

    fn result_view(&self) -> Element<'_, Message> {
        let result = self.result.as_ref().unwrap();
        let succeeded = self
            .result_items
            .iter()
            .filter(|item| item.status == ItemStatus::Succeeded)
            .count();
        let mut list = Column::new().spacing(8);
        for item in &self.result_items {
            let (label, color) = match item.status {
                ItemStatus::Succeeded => ("已完成", GREEN),
                ItemStatus::Failed => ("失败", WARNING),
                ItemStatus::Skipped => ("已跳过", WARNING),
                ItemStatus::NotStarted => ("未开始", MUTED),
                ItemStatus::InDoubt => ("结果未知", WARNING),
                ItemStatus::Started => ("已开始", ACCENT),
            };
            let mut details = column![
                row![
                    pill(label, color),
                    text(&item.path).size(11).width(Length::Fill)
                ]
                .spacing(10)
                .align_y(Alignment::Center)
            ]
            .spacing(6);
            if let Some(message) = &item.message {
                details = details.push(text(message).size(10).color(MUTED));
            }
            if let Some(code) = &item.code {
                details = details.push(text(code).size(10).color(MUTED));
            }
            list = list.push(
                container(details)
                    .padding(12)
                    .width(Length::Fill)
                    .style(|_| rounded_style(CARD, 8.0)),
            );
        }
        if self.result_cursor.is_some() {
            list = list.push(
                button("查看更多结果")
                    .padding(10)
                    .style(quiet_button)
                    .on_press(Message::MoreResults),
            );
        }
        let root = self
            .scan
            .as_ref()
            .map(|scan| scan.root_path.display.as_str())
            .unwrap_or("");
        let before = observed_capacity(result.get("space_before"), root);
        let after = observed_capacity(result.get("space_after"), root);
        let observation = match (before, after) {
            (Some(a), Some(b)) => format!("磁盘可用容量：{} → {}（系统观测）", bytes(a), bytes(b)),
            _ => "回收站操作不等于空间已释放；实际容量由系统重新观测。".into(),
        };
        let body = column![
            row![
                text("清理作业结果").size(24),
                Space::new().width(Length::Fill),
                button("×")
                    .style(quiet_button)
                    .on_press(Message::DismissResults)
            ]
            .align_y(Alignment::Center),
            text(format!(
                "当前已载入 {} 项结果，{} 项已完成。每项状态由本地服务记录。",
                self.result_items.len(),
                succeeded
            ))
            .size(13)
            .color(MUTED),
            text(observation).size(11).color(MUTED),
            scrollable(list).height(Length::Fill),
            text(
                if self
                    .active
                    .as_ref()
                    .is_some_and(|job| matches!(job.work, Work::Refresh))
                {
                    "正在重新扫描原位置，完成后会切换到新快照。"
                } else {
                    "原快照保持不变；请以重新扫描的结果查看当前文件状态。"
                }
            )
            .size(11)
            .color(ACCENT),
            button(text("返回空间分析").size(13))
                .padding([12, 20])
                .style(accent_button)
                .on_press(Message::DismissResults),
        ]
        .spacing(20);
        container(body)
            .padding(30)
            .width(800)
            .height(650)
            .style(|_| rounded_style(PANEL, 20.0))
            .into()
    }
}

fn source_hint(source: &Volume) -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        match source.mount_path.display.as_str() {
            "/" => Some("系统入口 · 包含数据目录的映射"),
            "/System/Volumes/Data" => Some("数据入口 · 用户文件与应用数据"),
            _ => None,
        }
    } else {
        None
    }
}

fn scan_size_summary(scan: &ScanSummary) -> String {
    if scan.root_id.is_none() {
        return match scan.state.as_deref() {
            Some("failed") => "扫描失败 · 未生成快照",
            Some("cancelled") => "扫描已取消 · 未生成快照",
            Some("interrupted") => "扫描已中断 · 未生成快照",
            Some("queued" | "running" | "cancel_requested") => "扫描中 · 尚未生成快照",
            _ => "尚未生成快照",
        }
        .into();
    }
    let state = if scan.known_stale {
        "待刷新"
    } else if scan.complete {
        "快照"
    } else {
        "部分结果"
    };
    format!("逻辑 {} · {state}", bytes(scan.logical_bytes))
}

fn observed_capacity(value: Option<&Value>, root: &str) -> Option<u64> {
    let volumes: Vec<Volume> = serde_json::from_value(value?.clone()).ok()?;
    volumes
        .iter()
        .filter(|volume| path_covers(&volume.mount_path.display, root))
        .max_by_key(|volume| volume.mount_path.display.len())
        .map(|volume| volume.available_bytes)
}
fn decode_items<T: serde::de::DeserializeOwned>(value: &Value) -> Vec<T> {
    value
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}
fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut amount = value as f64;
    let mut unit = 0;
    while amount >= 1000.0 && unit < UNITS.len() - 1 {
        amount /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else if amount >= 100.0 {
        format!("{amount:.0} {}", UNITS[unit])
    } else {
        format!("{amount:.1} {}", UNITS[unit])
    }
}
fn crop(value: &str, max: usize) -> String {
    if value.chars().count() > max {
        format!(
            "{}…",
            value
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        value.to_owned()
    }
}
fn path_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_owned())
}
fn node_covers(parent: &Node, child: &Node) -> bool {
    match (parent.path.to_path(), child.path.to_path()) {
        (Ok(parent_path), Ok(child_path)) => child_path.starts_with(parent_path),
        _ => parent.id == child.id,
    }
}

fn path_covers(parent: &str, child: &str) -> bool {
    std::path::Path::new(child).starts_with(std::path::Path::new(parent))
}
fn phase_label(phase: &str) -> &str {
    match phase {
        "scanning" | "enumerating" => "扫描中",
        "preparing" => "核对清单",
        "cleaning" | "executing" => "清理中",
        "completed" => "已完成",
        "saving" => "保存扫描结果",
        "indexing" => "整理扫描结果",
        "cancelled" => "已停止",
        "queued" => "等待中",
        _ => {
            if phase.is_empty() {
                "处理中"
            } else {
                phase
            }
        }
    }
}
fn state_label(state: &str) -> &str {
    match state {
        "queued" => "等待中",
        "running" => "进行中",
        "cancel_requested" => "已请求停止",
        "completed" => "已完成",
        "completed_with_errors" => "完成，含问题",
        "cancelled" => "已停止",
        "failed" => "失败",
        "interrupted" => "已中断",
        _ => state,
    }
}
fn job_label(work: &Work) -> &'static str {
    match work {
        Work::Scan => "正在扫描",
        Work::Prepare(_) => "正在准备清理计划",
        Work::Cleanup => "正在执行清理",
        Work::Refresh => "正在刷新扫描",
    }
}

/// Stable UTC display without an extra date crate. Service timestamps are Unix
/// milliseconds; include the timezone so a captured snapshot is unambiguous.
fn timestamp(ms: u64) -> String {
    let seconds = ms / 1000;
    let days = (seconds / 86400) as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        seconds % 86400 / 3600,
        seconds % 3600 / 60
    )
}
fn panel_style(color: Color) -> container::Style {
    container::Style {
        background: Some(Background::Color(color)),
        text_color: Some(WHITE),
        ..container::Style::default()
    }
}
fn rounded_style(color: Color, radius: f32) -> container::Style {
    container::Style {
        border: Border {
            radius: radius.into(),
            ..Border::default()
        },
        ..panel_style(color)
    }
}
fn quiet_button(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => CARD,
            _ => Color::TRANSPARENT,
        })),
        text_color: if status == button::Status::Disabled {
            Color::from_rgb(0.31, 0.34, 0.42)
        } else {
            MUTED
        },
        border: Border {
            radius: 8.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}
fn card_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = quiet_button(theme, status);
    style.background = Some(Background::Color(if status == button::Status::Hovered {
        Color::from_rgb(0.14, 0.16, 0.23)
    } else {
        CARD
    }));
    style.text_color = WHITE;
    style.border.radius = 12.0.into();
    style
}
fn selected_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = card_button(theme, status);
    style.background = Some(Background::Color(Color::from_rgb(0.16, 0.17, 0.29)));
    style.text_color = WHITE;
    style
}
fn accent_button(_theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered => Color::from_rgb(0.51, 0.51, 0.97),
            button::Status::Disabled => Color::from_rgb(0.19, 0.21, 0.33),
            _ => Color::from_rgb(0.38, 0.39, 0.88),
        })),
        text_color: if status == button::Status::Disabled {
            MUTED
        } else {
            WHITE
        },
        border: Border {
            radius: 9.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}
fn pill<'a>(label: impl Into<String>, color: Color) -> Element<'a, Message> {
    container(text(label.into()).size(10).color(color))
        .padding([5, 9])
        .style(move |_| rounded_style(Color::from_rgba(color.r, color.g, color.b, 0.10), 6.0))
        .into()
}
