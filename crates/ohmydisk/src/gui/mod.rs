//! Native desktop frontend. Filesystem work belongs to the shared local service;
//! the UI only owns navigation, an uncommitted collection, and explicit approval.
mod collector_interaction;
mod scan_clock;
mod space_view;
mod sunburst;
mod views;
mod window_chrome;

use collector_interaction as interaction;

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::Duration,
};

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
use ohmydisk_core::{SpaceAccounting, SpaceObservation};
use ohmydisk_service::ipc::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use space_view::{Capacity, RootMode, default_root_mode, node_bytes, partly_measured, shared_zero};

const BG: Color = Color::from_rgb(0.055, 0.063, 0.09);
const PANEL: Color = Color::from_rgb(0.075, 0.086, 0.12);
const CARD: Color = Color::from_rgb(0.105, 0.119, 0.165);
const MUTED: Color = Color::from_rgb(0.57, 0.61, 0.72);
const WHITE: Color = Color::from_rgb(0.90, 0.92, 0.98);
const ACCENT: Color = Color::from_rgb(0.48, 0.50, 1.0);
const GREEN: Color = Color::from_rgb(0.39, 0.79, 0.67);
const WARNING: Color = Color::from_rgb(0.96, 0.69, 0.39);
const APP_NAME: &str = "OhMyDisk";
const TAGLINE: &str = "空间，清晰可见";

pub fn run(data_dir: PathBuf) -> iced::Result {
    let mut app = iced::application(move || App::new(data_dir.clone()), App::update, App::view)
        .title(|_: &App| format!("{APP_NAME} {TAGLINE}"))
        .subscription(App::subscription)
        .theme(Theme::Dark)
        .default_font(chinese_font())
        .window(window_chrome::settings())
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
    space_accounting: Option<SpaceAccounting>,
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    known_stale: bool,
    refresh_job_id: Option<String>,
    job_id: Option<String>,
    state: Option<String>,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    started_at: Option<u64>,
}

#[derive(Clone, Debug)]
struct Location {
    id: String,
    name: String,
    path: String,
    bytes: Option<u64>,
    measurement_partial: bool,
}

#[derive(Clone, Debug, Default)]
struct Preview {
    items: Vec<Node>,
    next_cursor: Option<String>,
}

#[derive(Clone, Debug)]
enum Response {
    Sources,
    Scans,
    StartedScan,
    Poll {
        id: String,
    },
    Scan {
        epoch: u64,
    },
    Children {
        epoch: u64,
        append: bool,
    },
    Descendants {
        epoch: u64,
    },
    Preview {
        scan_epoch: u64,
        hover_epoch: u64,
        owner: String,
    },
    PlanStarted(CleanupMode),
    Plan {
        append: bool,
    },
    Executed,
    CleanupResult {
        append: bool,
    },
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
    clock: scan_clock::Clock,
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
    WindowChrome(window_chrome::Message),
    CollectorInteraction(interaction::Event),
    ToggleCompactPreview,
    Connected(Result<Client, String>),
    Response(Response, Result<Value, String>),
    Overview,
    SetRootMode(RootMode),
    ToggleCollector,
    ShowSpaceInfo(sunburst::Target),
    ChartHover(Option<sunburst::Target>),
    PreviewReady { epoch: u64, owner: String },
    ClearPreview { epoch: u64 },
    PreviewPane(bool),
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
    ChartReleased { segment: Option<sunburst::Segment> },
    Collect(String),
    Remove(String),
    ClearCollection,
    Prepare(CleanupMode),
    DismissReview,
    ReviewExpired { plan_id: String },
    ConfirmText(String),
    Execute,
    MorePlan,
    MoreResults,
    DismissResults,
    RevealInScan { scan_id: String, node_id: String },
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
    chrome: window_chrome::State,
    compact_preview: bool,
    collector_interaction: interaction::Controller,
    collector_panel: interaction::PanelState,
    client: Option<Client>,
    data_dir: PathBuf,
    connecting: bool,
    overview: bool,
    root_mode: RootMode,
    chart_target: Option<sunburst::Target>,
    space_info: Option<sunburst::Target>,
    preview_owner: Option<String>,
    preview_cache: HashMap<String, Preview>,
    preview_epoch: u64,
    preview_loading: bool,
    preview_error: Option<String>,
    preview_pane_hover: bool,
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
        let task = Task::batch([
            Self::connect(data_dir.clone()),
            window_chrome::State::initial_task().map(Message::WindowChrome),
        ]);
        (
            Self {
                chrome: window_chrome::State::default(),
                compact_preview: false,
                collector_interaction: interaction::Controller::default(),
                collector_panel: interaction::PanelState::default(),
                client: None,
                data_dir,
                connecting: true,
                overview: true,
                root_mode: RootMode::Directory,
                chart_target: None,
                space_info: None,
                preview_owner: None,
                preview_cache: HashMap::new(),
                preview_epoch: 0,
                preview_loading: false,
                preview_error: None,
                preview_pane_hover: false,
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
                let client = Client::connect_or_spawn(path)
                    .await
                    .map_err(|error| error.to_string())?;
                let capabilities = client
                    .call("get_capabilities", json!({}))
                    .await
                    .map_err(|error| format!("无法读取后台服务能力：{error}"))?;
                require_occupancy_backend(&capabilities)?;
                Ok(client)
            },
            Message::Connected,
        )
    }
    fn review_expiry_task(plan_id: String, expires_at: u64) -> Task<Message> {
        // This is a plan-bound one-shot, not an application-wide redraw timer.
        let delay = expires_at
            .saturating_sub(ohmydisk_core::now_ms())
            .saturating_add(1);
        Task::perform(
            async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                plan_id
            },
            |plan_id| Message::ReviewExpired { plan_id },
        )
    }
    fn mark_review_expired(&mut self) {
        self.confirm_text.clear();
        self.banner = Some(
            if self.busy {
                "清理计划已过期；已提交的操作请以后台结果为准。"
            } else {
                "清理计划已过期，请返回检查并重新准备计划。"
            }
            .into(),
        );
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
        let jobs = if self.active.is_some() {
            iced::time::every(Duration::from_millis(650)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            window_chrome::State::subscription().map(Message::WindowChrome),
            jobs,
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        if matches!(
            &message,
            Message::Overview
                | Message::SetRootMode(_)
                | Message::ToggleCompactPreview
                | Message::ToggleCollector
                | Message::PickFolder
                | Message::OpenScan(_)
                | Message::Navigate(_)
                | Message::Back
                | Message::Forward
                | Message::Up
                | Message::Rescan
                | Message::Prepare(_)
                | Message::DismissReview
                | Message::DismissResults
        ) {
            self.collector_interaction.cancel();
        }
        match message {
            Message::WindowChrome(message) => {
                if matches!(
                    &message,
                    window_chrome::Message::WindowEvent(
                        _,
                        iced::window::Event::Unfocused
                            | iced::window::Event::Closed
                            | iced::window::Event::Resized(_)
                            | iced::window::Event::Rescaled(_)
                    )
                ) {
                    self.collector_interaction.cancel();
                }
                let task = self.chrome.update(message).map(Message::WindowChrome);
                if !self.compact_layout() && self.compact_preview {
                    self.compact_preview = false;
                    self.reset_preview();
                }
                return task;
            }
            Message::CollectorInteraction(event) => match event {
                interaction::Event::HeightChanged(height) => self.collector_panel.resize(height),
                interaction::Event::Dropped(entry) => {
                    if self.interaction_scope().as_ref() == Some(&entry.scope)
                        && self.collection_interaction_enabled()
                        && self.collect_node(&entry.scope.scan_id, entry.node)
                    {
                        self.collector_panel.open();
                    }
                }
            },
            Message::ToggleCompactPreview => {
                if !self.compact_layout() {
                    return Task::none();
                }
                self.compact_preview = !self.compact_preview;
                if self.compact_preview {
                    // An explicit compact preview stays selected while the
                    // pointer travels from the hidden chart to the list.
                    self.preview_pane_hover = true;
                } else {
                    self.reset_preview();
                }
            }
            Message::Overview => {
                self.overview = true;
                self.reset_preview();
                return self.reload();
            }
            Message::SetRootMode(mode) => {
                if self.is_root() && self.capacity().is_some() {
                    self.root_mode = mode;
                    self.space_info = None;
                    self.reset_preview();
                    self.rebuild_chart();
                }
            }
            Message::ToggleCollector => self.collector_panel.toggle(),
            Message::ShowSpaceInfo(target) => {
                self.space_info = if self.space_info.as_ref() == Some(&target) {
                    None
                } else {
                    Some(target)
                };
                self.reset_preview();
            }
            Message::ChartHover(target) => return self.chart_hover(target),
            Message::PreviewReady { epoch, owner } => {
                if epoch == self.preview_epoch
                    && self.preview_owner.as_ref() == Some(&owner)
                    && !self.preview_cache.contains_key(&owner)
                    && (self.chart_target.is_some() || self.preview_pane_hover)
                {
                    if let Some(scan) = &self.scan {
                        return self.request("list_children", json!({"scan_id":scan.scan_id,"node_id":owner,"limit":100,"size_metric":"attributed"}),
                            Response::Preview { scan_epoch:self.epoch, hover_epoch:epoch, owner });
                    }
                }
            }
            Message::ClearPreview { epoch } => {
                if epoch == self.preview_epoch
                    && self.chart_target.is_none()
                    && !self.preview_pane_hover
                    && !(self.compact_layout() && self.compact_preview)
                {
                    self.reset_preview();
                }
            }
            Message::PreviewPane(entered) => {
                self.preview_pane_hover =
                    entered || (self.compact_layout() && self.compact_preview);
                if !self.preview_pane_hover && self.chart_target.is_none() {
                    self.reset_preview();
                }
            }
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
                let response_epoch = match &response {
                    Response::Scan { epoch }
                    | Response::Children { epoch, .. }
                    | Response::Descendants { epoch } => Some(*epoch),
                    _ => None,
                };
                if response_epoch.is_some_and(|epoch| epoch != self.epoch) {
                    return Task::none();
                }
                if let Response::Poll { .. } = &response {
                    self.polling = false;
                }
                match result {
                    Ok(value) => return self.receive(response, value),
                    Err(error) => {
                        if let Response::Preview {
                            scan_epoch,
                            hover_epoch,
                            owner,
                        } = &response
                        {
                            if *scan_epoch == self.epoch
                                && *hover_epoch == self.preview_epoch
                                && self.preview_owner.as_ref() == Some(owner)
                            {
                                self.preview_loading = false;
                                self.preview_error = Some(error);
                            }
                            return Task::none();
                        }
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
                        return self.request("list_children", json!({"scan_id":scan.scan_id,"node_id":location.id,"limit":100,"cursor":cursor,"size_metric":"attributed"}), Response::Children { epoch: self.epoch, append: true });
                    }
                }
            }
            Message::Hover(id) => self.hover = id,
            Message::ChartReleased { segment } => {
                if let Some(segment) = segment {
                    match segment.target {
                        sunburst::Target::Node(id) => return self.navigate(&id, true),
                        sunburst::Target::Aggregate { owner_id } => {
                            if self
                                .location
                                .as_ref()
                                .is_some_and(|location| location.id == owner_id)
                            {
                                self.banner = Some(
                                    "右侧列表包含较小项目，可继续载入。聚合扇区不能加入收集区。"
                                        .into(),
                                );
                            } else {
                                return self.navigate(&owner_id, true);
                            }
                        }
                        target => self.space_info = Some(target),
                    }
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
            Message::ReviewExpired { plan_id } => {
                if let Some(plan) = self.review.as_ref().filter(|plan| plan.id == plan_id) {
                    if ohmydisk_core::now_ms() > plan.expires_at {
                        self.mark_review_expired();
                    } else {
                        // A backwards wall-clock adjustment must not lose the deadline wakeup.
                        return Self::review_expiry_task(plan_id, plan.expires_at);
                    }
                }
            }
            Message::ConfirmText(value) => self.confirm_text = value,
            Message::Execute => {
                if self
                    .review
                    .as_ref()
                    .is_some_and(|plan| ohmydisk_core::now_ms() > plan.expires_at)
                {
                    self.mark_review_expired();
                    return Task::none();
                }
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
            Message::RevealInScan { scan_id, node_id } => {
                return self.request(
                    "reveal_node",
                    json!({"scan_id":scan_id,"node_id":node_id}),
                    Response::Reveal,
                );
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
                        clock: scan_clock::Clock::default(),
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
                                "{} · {} 个文件 · {} 个文件夹 · {}",
                                phase_label(&job.progress.phase),
                                job.progress.files,
                                job.progress.directories,
                                crop(&job.progress.current_path, 56)
                            );
                            active.clock.observe(job.elapsed_ms.unwrap_or_else(|| {
                                ohmydisk_core::now_ms().saturating_sub(job.created_at)
                            }));
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
                            self.overview = true;
                            self.loading = false;
                            let status = scan_size_summary(&scan);
                            let clock = clock_for_scan(&scan);
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
                                        clock,
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
                            bytes: scan
                                .space_accounting
                                .as_ref()
                                .and_then(Capacity::reconciled)
                                .map(|space| space.attributed),
                            measurement_partial: scan.space_accounting.as_ref().is_some_and(
                                |accounting| accounting.measurement_status != "complete",
                            ),
                        };
                        if scan.known_stale && self.active.is_none() {
                            if let Some(id) = &scan.refresh_job_id {
                                self.active = Some(ActiveJob {
                                    id: id.clone(),
                                    work: Work::Refresh,
                                    state: "queued".into(),
                                    progress: "正在取得后台刷新状态…".into(),
                                    clock: scan_clock::Clock::default(),
                                });
                            }
                        }
                        self.root_mode = default_root_mode(
                            &scan.root_path.display,
                            &self.sources,
                            cfg!(target_os = "windows"),
                        );
                        self.breadcrumbs.clear();
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
            Response::Preview {
                scan_epoch,
                hover_epoch,
                owner,
            } if scan_epoch == self.epoch => {
                let preview = Preview {
                    items: decode_items(&value),
                    next_cursor: string(&value, "next_cursor"),
                };
                if self.preview_cache.len() >= 64 {
                    self.preview_cache.retain(|id, _| {
                        self.preview_owner.as_ref() == Some(id)
                            || self
                                .location
                                .as_ref()
                                .is_some_and(|location| &location.id == id)
                    });
                }
                self.preview_cache.insert(owner.clone(), preview);
                if hover_epoch == self.preview_epoch && self.preview_owner.as_ref() == Some(&owner)
                {
                    self.preview_loading = false;
                    self.preview_error = None;
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
                if let Some(location) = &self.location {
                    self.preview_cache.insert(
                        location.id.clone(),
                        Preview {
                            items: self.children.clone(),
                            next_cursor: self.next_cursor.clone(),
                        },
                    );
                }
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
                        clock: scan_clock::Clock::default(),
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
                    let plan = ReviewPlan {
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
                    };
                    let expiry = Self::review_expiry_task(plan.id.clone(), plan.expires_at);
                    self.review = Some(plan);
                    return expiry;
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
                        clock: scan_clock::Clock::default(),
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
                            clock: scan_clock::Clock::default(),
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
        self.collector_interaction.cancel();
        self.epoch += 1;
        self.loading = true;
        self.overview = false;
        self.reset_preview();
        self.preview_cache.clear();
        self.request(
            "get_scan",
            json!({"scan_id": id}),
            Response::Scan { epoch: self.epoch },
        )
    }
    fn find_node(&self, id: &str) -> Option<&Node> {
        let collection_is_current = self
            .scan
            .as_ref()
            .is_some_and(|scan| self.draft_scan_id.as_ref() == Some(&scan.scan_id));
        self.children
            .iter()
            .chain(self.chart_nodes.iter())
            .chain(
                self.preview_cache
                    .values()
                    .flat_map(|preview| preview.items.iter()),
            )
            .chain(self.collection.iter().filter(|_| collection_is_current))
            .find(|node| node.id == id)
    }
    fn scan_time_label(&self, scan: &ScanSummary) -> String {
        if let Some(job) = self.active.as_ref().filter(|job| {
            scan.job_id.as_ref() == Some(&job.id) && matches!(job.work, Work::Scan | Work::Refresh)
        }) {
            return format!(
                "已用时 {}",
                scan_clock::format_duration(job.clock.elapsed_ms())
            );
        }
        scan_time_summary(scan)
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
                bytes: node_bytes(node),
                measurement_partial: partly_measured(node),
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
        self.collector_interaction.cancel();
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
                        bytes: node_bytes(node),
                        measurement_partial: partly_measured(node),
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
        self.reset_preview();
        self.space_info = None;
        self.location = Some(location.clone());
        self.children.clear();
        self.chart_nodes.clear();
        self.segments.clear();
        self.next_cursor = None;
        self.hover = None;
        self.loading = true;
        self.epoch += 1;
        let epoch = self.epoch;
        if self.capacity().is_none() {
            self.loading = false;
            self.rebuild_chart();
            return Task::none();
        }
        Task::batch([
            self.request("list_children", json!({"scan_id":scan_id,"node_id":location.id,"limit":100,"size_metric":"attributed"}), Response::Children { epoch, append:false }),
            self.request("query_nodes", json!({"scan_id":scan_id,"scope_node_id":location.id,"descendants":true,"limit":500,"size_metric":"attributed"}), Response::Descendants { epoch }),
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
        nodes.sort_by(|a, b| node_bytes(b).cmp(&node_bytes(a)));
        self.segments = if let Some(capacity) = self.capacity() {
            if self.display_mode() == RootMode::Volume {
                sunburst::capacity_layout(&location.id, capacity, &nodes)
            } else if let Some(amount) = location.bytes.filter(|amount| *amount <= capacity.total) {
                sunburst::layout(&location.id, amount, &nodes)
            } else {
                Vec::new()
            }
        } else if let Some((total, available)) = self.observed_totals() {
            sunburst::overview_layout(total, available)
        } else {
            Vec::new()
        };
    }
    pub(super) fn interaction_scope(&self) -> Option<interaction::Scope> {
        if self.overview {
            return None;
        }
        self.scan.as_ref().map(|scan| interaction::Scope {
            scan_id: scan.scan_id.clone(),
            epoch: self.epoch,
        })
    }

    pub(super) fn collection_interaction_enabled(&self) -> bool {
        !self.overview
            && self.capacity().is_some()
            && self.review.is_none()
            && self.result.is_none()
            && !self.busy
            && !self
                .active
                .as_ref()
                .is_some_and(|job| matches!(job.work, Work::Cleanup))
    }

    pub(super) fn drag_entry(&self, node: &Node) -> Option<interaction::Entry> {
        if !self.collection_interaction_enabled()
            || !node.complete
            || self
                .scan
                .as_ref()
                .is_some_and(|scan| scan.root_id.as_deref() == Some(node.id.as_str()))
        {
            return None;
        }
        Some(interaction::Entry {
            scope: self.interaction_scope()?,
            node: node.clone(),
        })
    }

    fn collect(&mut self, id: &str) {
        let Some(scan_id) = self.scan.as_ref().map(|scan| scan.scan_id.clone()) else {
            return;
        };
        let Some(node) = self.find_node(id).cloned() else {
            return;
        };
        self.collect_node(&scan_id, node);
    }

    /// A plus click and a captured drag use the same collection rules. The node
    /// is from an immutable snapshot; drag scope is checked before entering.
    fn collect_node(&mut self, scan_id: &str, node: Node) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|job| matches!(job.work, Work::Cleanup))
            || self.busy
        {
            self.banner = Some("清理作业进行中，请等待逐项结果后再修改收集区。".into());
            return false;
        }
        let Some(scan) = &self.scan else {
            return false;
        };
        if scan.scan_id != scan_id || self.review.is_some() || self.result.is_some() {
            return false;
        }
        if self
            .draft_scan_id
            .as_ref()
            .is_some_and(|draft| draft != &scan.scan_id)
        {
            self.banner =
                Some("收集区来自另一份扫描。请先处理或清空，再加入当前扫描的项目。".into());
            return false;
        }
        if Some(node.id.as_str()) == scan.root_id.as_deref() {
            self.banner = Some("扫描根不能作为清理目标，请选择里面的文件或文件夹。".into());
            return false;
        }
        if !node.complete {
            self.banner = Some("该项目的扫描不完整，请先处理访问问题并重新扫描。".into());
            return false;
        }
        if self
            .collection
            .iter()
            .any(|entry| node_covers(entry, &node))
        {
            return true;
        }
        self.collection.retain(|entry| !node_covers(&node, entry));
        if self.collection.len() >= 500 {
            self.banner = Some("单个清理计划最多选择 500 个目标；可选择其父目录。".into());
            return false;
        }
        self.draft_scan_id = Some(scan.scan_id.clone());
        self.collection.push(node);
        true
    }

    fn review_view<'a>(&'a self, plan: &'a ReviewPlan) -> Element<'a, Message> {
        let permanent = plan.mode == CleanupMode::Permanent;
        let compact = self.compact_layout() || self.chrome.size.height < 760.0;
        let dimensions = self.modal_dimensions(760.0, 680.0);
        let mut targets = Column::new().spacing(9);
        for node in &plan.targets {
            targets = targets.push(
                container(
                    column![
                        row![
                            text(crop(&node.name, 64)).size(13).width(Length::Fill),
                            text(format!("文件逻辑大小 {}", bytes(node.logical_bytes)))
                                .size(12)
                                .color(MUTED)
                        ]
                        .spacing(12),
                        text(&node.path.display)
                            .size(11)
                            .width(Length::Fill)
                            .wrapping(text::Wrapping::Glyph)
                            .color(MUTED)
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
                detail = detail.push(
                    text(path)
                        .size(10)
                        .width(Length::Fill)
                        .wrapping(text::Wrapping::Glyph)
                        .color(MUTED),
                );
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
        let heading = row![
            text(if permanent {
                "确认永久删除"
            } else {
                "确认移入回收站"
            })
            .size(if compact { 19 } else { 24 }),
            Space::new().width(Length::Fill),
            button("×")
                .style(quiet_button)
                .on_press_maybe((!self.busy).then_some(Message::DismissReview))
        ]
        .align_y(Alignment::Center);
        let mut content = column![
            text(if permanent {
                "这些项目将永久删除，无法通过回收站恢复。请核对下面的路径与操作方式。"
            } else {
                "下面的项目将移入系统回收站。回收站占用仍可能保留，实际可用容量以清理后观测为准。"
            })
            .size(if compact { 11 } else { 13 })
            .width(Length::Fill)
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
                    "{} 个目标 · 文件逻辑总量 {}",
                    plan.targets.len(),
                    bytes(plan.bytes)
                ))
                .size(13),
                Space::new().width(Length::Fill)
            ]
            .spacing(if compact { 8 } else { 14 })
            .align_y(Alignment::Center),
            detail,
            text(format!(
                "计划有效期至 {} · 文件发生变化时服务会跳过并报告",
                timestamp(plan.expires_at)
            ))
            .size(10)
            .color(MUTED),
        ]
        .spacing(if compact { 7 } else { 18 });
        if permanent {
            content = content.push(
                text_input("输入「永久删除」确认此操作方式", &self.confirm_text)
                    .on_input(Message::ConfirmText)
                    .padding(if compact { 9 } else { 13 })
                    .size(13),
            );
        }
        if let Some(banner) = &self.banner {
            content = content.push(text(banner).size(11).color(WARNING));
        }
        let enabled = !self.busy
            && ohmydisk_core::model::now_ms() <= plan.expires_at
            && !plan.targets.is_empty()
            && (!permanent || self.confirm_text == "永久删除");
        let actions = row![
            button(text("返回检查").size(13))
                .padding(if compact { [9, 12] } else { [12, 18] })
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
            .padding(if compact { [9, 12] } else { [12, 20] })
            .style(if permanent {
                button::danger
            } else {
                accent_button
            })
            .on_press_maybe(enabled.then_some(Message::Execute))
        ]
        .spacing(12);
        let body = column![heading, scrollable(content).height(Length::Fill), actions]
            .spacing(if compact { 7 } else { 18 });
        container(body)
            .padding(if compact { 12 } else { 30 })
            .width(dimensions.width)
            .height(dimensions.height)
            .style(|_| rounded_style(PANEL, 20.0))
            .into()
    }

    fn result_view(&self) -> Element<'_, Message> {
        let result = self.result.as_ref().unwrap();
        let compact = self.compact_layout() || self.chrome.size.height < 760.0;
        let dimensions = self.modal_dimensions(800.0, 650.0);
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
                    text(&item.path)
                        .size(11)
                        .width(Length::Fill)
                        .wrapping(text::Wrapping::Glyph)
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
        let heading = row![
            text("清理作业结果").size(if compact { 19 } else { 24 }),
            Space::new().width(Length::Fill),
            button("×")
                .style(quiet_button)
                .on_press(Message::DismissResults)
        ]
        .align_y(Alignment::Center);
        let content = column![
            text(format!(
                "当前已载入 {} 项结果，{} 项已完成。每项状态由本地服务记录。",
                self.result_items.len(),
                succeeded
            ))
            .size(13)
            .color(MUTED),
            text(observation).size(11).color(MUTED),
            list,
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
        ]
        .spacing(if compact { 8 } else { 20 });
        let body = column![
            heading,
            scrollable(content).height(Length::Fill),
            button(text("返回空间分析").size(13))
                .padding([12, 20])
                .style(accent_button)
                .on_press(Message::DismissResults),
        ]
        .spacing(if compact { 8 } else { 20 });
        container(body)
            .padding(if compact { 12 } else { 30 })
            .width(dimensions.width)
            .height(dimensions.height)
            .style(|_| rounded_style(PANEL, 20.0))
            .into()
    }
}

fn require_occupancy_backend(capabilities: &Value) -> Result<(), String> {
    if capabilities
        .get("size_metrics")
        .and_then(Value::as_array)
        .is_some_and(|metrics| {
            metrics
                .iter()
                .any(|metric| metric.as_str() == Some("attributed"))
        })
    {
        Ok(())
    } else {
        Err("连接的是旧后台服务，暂不支持占用测量。请在当前作业结束后退出旧版 GUI 和 MCP 客户端，等待后台空闲退出，再点「重新连接」。".into())
    }
}

#[cfg(test)]
mod backend_tests {
    use super::*;
    #[test]
    fn connection_requires_the_occupancy_metric_without_requiring_a_native_provider() {
        assert!(
            require_occupancy_backend(&json!({"size_metrics":["logical","allocated"]}))
                .unwrap_err()
                .contains("旧后台服务")
        );
        assert!(require_occupancy_backend(&json!({})).is_err());
        assert!(
            require_occupancy_backend(
                &json!({"size_metrics":["attributed"],"provider":"unsupported"})
            )
            .is_ok()
        );
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

fn clock_for_scan(scan: &ScanSummary) -> scan_clock::Clock {
    let mut clock = scan_clock::Clock::default();
    if let Some(elapsed) = scan.elapsed_ms.or_else(|| {
        scan.started_at
            .map(|start| ohmydisk_core::now_ms().saturating_sub(start))
    }) {
        clock.observe(elapsed);
    }
    clock
}

fn scan_time_summary(scan: &ScanSummary) -> String {
    let running = matches!(
        scan.state.as_deref(),
        Some("queued" | "running" | "cancel_requested")
    );
    match scan.elapsed_ms {
        Some(elapsed) => format!(
            "{} {}",
            if running { "已用时" } else { "总耗时" },
            scan_clock::format_duration(elapsed)
        ),
        None if running => "正在计时".into(),
        None => "总耗时未记录".into(),
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
    match scan
        .space_accounting
        .as_ref()
        .and_then(Capacity::reconciled)
    {
        Some(space) => format!(
            "已用 / 保留 {} · {state}",
            bytes(space.total - space.available)
        ),
        None => format!("需要重新测量占用 · {state}"),
    }
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
