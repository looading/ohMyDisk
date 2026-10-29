//! Source overview and the focused capacity workspace share one navigation state.
use super::*;
use iced::widget::column;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CollectionContext {
    Current,
    OtherScan,
    ReviewRequired,
}

impl App {
    pub(super) fn capacity(&self) -> Option<Capacity> {
        self.scan
            .as_ref()?
            .space_accounting
            .as_ref()
            .and_then(Capacity::reconciled)
    }
    pub(super) fn is_root(&self) -> bool {
        self.location
            .as_ref()
            .zip(self.scan.as_ref())
            .is_some_and(|(location, scan)| Some(location.id.as_str()) == scan.root_id.as_deref())
    }
    pub(super) fn display_mode(&self) -> RootMode {
        if self.is_root() && self.capacity().is_some() {
            self.root_mode
        } else {
            RootMode::Directory
        }
    }
    pub(super) fn root_amount(&self) -> Option<u64> {
        if let Some(capacity) = self.capacity() {
            if self.display_mode() == RootMode::Volume {
                Some(capacity.total - capacity.available)
            } else {
                self.scan
                    .as_ref()?
                    .space_accounting
                    .as_ref()
                    .filter(|accounting| accounting.measurement_status != "unavailable")
                    .map(|_| capacity.attributed)
            }
        } else {
            self.observed_totals()
                .map(|(total, available)| total - available)
        }
    }
    pub(super) fn scope_switch(&self) -> Element<'_, Message> {
        if !self.is_root() || self.capacity().is_none() {
            return Space::new().into();
        }
        let mut modes = Row::new().spacing(3);
        for (mode, label) in [
            (RootMode::Volume, "卷容量"),
            (RootMode::Directory, "目录内容"),
        ] {
            let selected = self.root_mode == mode;
            modes = modes.push(
                button(text(label).size(11))
                    .padding([7, 11])
                    .style(move |theme, status| {
                        if selected {
                            selected_button(theme, status)
                        } else {
                            quiet_button(theme, status)
                        }
                    })
                    .on_press(Message::SetRootMode(mode)),
            );
        }
        container(modes)
            .padding(3)
            .style(|_| rounded_style(PANEL, 9.0))
            .into()
    }
    pub(super) fn space_observation(&self) -> Option<&SpaceObservation> {
        if let Some(accounting) = self
            .scan
            .as_ref()
            .and_then(|scan| scan.space_accounting.as_ref())
        {
            return Some(space_view::observation(accounting));
        }
        let root = &self.scan.as_ref()?.root_path.display;
        self.sources
            .iter()
            .filter(|source| path_covers(&source.mount_path.display, root))
            .max_by_key(|source| source.mount_path.display.len())?
            .space_observation
            .as_ref()
    }
    pub(super) fn observed_totals(&self) -> Option<(u64, u64)> {
        let observation = self.space_observation()?;
        (observation.total_bytes > 0
            && observation.available_bytes <= observation.total_bytes
            && observation.capacity_scope != "caller_view")
            .then_some((observation.total_bytes, observation.available_bytes))
    }
    pub(super) fn current_node_bytes(&self, node: &Node) -> Option<u64> {
        let capacity = self.capacity()?;
        node_bytes(node).filter(|bytes| *bytes <= capacity.total)
    }
    pub(super) fn current_location_bytes(&self) -> Option<u64> {
        let capacity = self.capacity()?;
        self.location
            .as_ref()?
            .bytes
            .filter(|bytes| *bytes <= capacity.total)
    }
    pub(super) fn collection_context(&self) -> CollectionContext {
        let (Some(draft), Some(scan)) = (&self.draft_scan_id, &self.scan) else {
            return CollectionContext::ReviewRequired;
        };
        if draft != &scan.scan_id {
            CollectionContext::OtherScan
        } else if self.capacity().is_some() {
            CollectionContext::Current
        } else {
            CollectionContext::ReviewRequired
        }
    }
    pub(super) fn current_collection(&self) -> &[Node] {
        if self.collection_context() == CollectionContext::Current {
            &self.collection
        } else {
            &[]
        }
    }
    pub(super) fn chart_collection_ids(&self) -> Vec<String> {
        self.current_collection()
            .iter()
            .map(|node| node.id.clone())
            .collect()
    }
    pub(super) fn row_is_collected(&self, node: &Node) -> bool {
        self.current_collection()
            .iter()
            .any(|entry| node_covers(entry, node))
    }
    pub(super) fn current_reveal_message(&self, node_id: &str) -> Option<Message> {
        self.scan.as_ref().map(|scan| Message::RevealInScan {
            scan_id: scan.scan_id.clone(),
            node_id: node_id.to_owned(),
        })
    }
    pub(super) fn collection_node_amount(&self, node: &Node) -> String {
        match self.collection_context() {
            CollectionContext::Current => self.node_amount(node),
            CollectionContext::OtherScan => "其他扫描 · 占用待复核".into(),
            CollectionContext::ReviewRequired => "占用待复核".into(),
        }
    }
    pub(super) fn node_amount(&self, node: &Node) -> String {
        let Some(capacity) = self.capacity() else {
            return "占用待复核".into();
        };
        match node_bytes(node) {
            None => "未测量".into(),
            Some(amount) if amount > capacity.total => "测量不一致".into(),
            Some(_) if shared_zero(node) => "共享 · 归属 0 B".into(),
            Some(amount) if partly_measured(node) => format!("至少 {} · 部分", bytes(amount)),
            Some(amount) => bytes(amount),
        }
    }
    pub(super) fn collection_amount(&self) -> String {
        match self.collection_context() {
            CollectionContext::OtherScan => return "来自其他扫描 · 占用待复核".into(),
            CollectionContext::ReviewRequired => return "占用待复核".into(),
            CollectionContext::Current => {}
        }
        let mut total = 0_u64;
        let mut partial = false;
        let mut measured = false;
        for node in &self.collection {
            let Some(amount) = node_bytes(node) else {
                partial = true;
                continue;
            };
            let Some(sum) = total.checked_add(amount) else {
                return "测量不一致".into();
            };
            total = sum;
            measured = true;
            partial |= partly_measured(node);
        }
        if self
            .capacity()
            .is_some_and(|capacity| total > capacity.total)
        {
            return "测量不一致".into();
        }
        if !measured && partial {
            "未测量".into()
        } else if partial {
            format!("{} · 部分测量", bytes(total))
        } else {
            bytes(total)
        }
    }
    pub(super) fn reset_preview(&mut self) {
        self.preview_epoch = self.preview_epoch.wrapping_add(1);
        self.preview_owner = None;
        self.preview_loading = false;
        self.preview_error = None;
        self.preview_pane_hover = false;
        self.chart_target = None;
        self.hover = None;
    }
    pub(super) fn chart_hover(&mut self, target: Option<sunburst::Target>) -> Task<Message> {
        if self.chart_target == target {
            return Task::none();
        }
        self.chart_target = target.clone();
        self.hover = target
            .as_ref()
            .and_then(|target| target.node_id().map(str::to_owned));
        if target.is_some() {
            self.preview_epoch = self.preview_epoch.wrapping_add(1);
        }
        let epoch = self.preview_epoch;
        if target.is_none() {
            return Task::perform(
                async move {
                    tokio::time::sleep(Duration::from_millis(140)).await;
                    epoch
                },
                |epoch| Message::ClearPreview { epoch },
            );
        }
        self.preview_error = None;
        let owner = self
            .hover
            .as_deref()
            .and_then(|id| self.find_node(id))
            .filter(|node| node.kind == NodeKind::Directory)
            .map(|node| node.id.clone());
        self.preview_owner = owner.clone();
        self.preview_loading = owner
            .as_ref()
            .is_some_and(|owner| !self.preview_cache.contains_key(owner));
        if let Some(owner) = owner.filter(|owner| !self.preview_cache.contains_key(owner)) {
            return Task::perform(
                async move {
                    tokio::time::sleep(Duration::from_millis(130)).await;
                    (epoch, owner)
                },
                |(epoch, owner)| Message::PreviewReady { epoch, owner },
            );
        }
        Task::none()
    }
    pub(super) fn view(&self) -> Element<'_, Message> {
        let mut body = column![self.header(), rule::horizontal(1)].spacing(0);
        if let Some(banner) = &self.banner {
            body = body.push(
                container(
                    row![
                        text(banner).size(12).color(WARNING),
                        Space::new().width(Length::Fill),
                        button("×")
                            .style(quiet_button)
                            .on_press(Message::ClearBanner)
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                )
                .padding([9, 24])
                .width(Length::Fill)
                .style(|_| panel_style(Color::from_rgb(0.17, 0.13, 0.105))),
            );
        }
        if self.overview || self.scan.is_none() {
            body = body.push(self.sources_view());
        } else {
            body = body.push(column![self.navigation(), self.analysis()].height(Length::Fill));
        }
        body = body.push(self.collector());
        let main: Element<'_, Message> = container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| panel_style(BG))
            .into();
        let overlay = if let Some(plan) = &self.review {
            Some(self.review_view(plan))
        } else if self.result.is_some() {
            Some(self.result_view())
        } else {
            None
        };
        if let Some(overlay) = overlay {
            stack![
                main,
                iced::widget::opaque(
                    container(overlay)
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
    pub(super) fn header(&self) -> Element<'_, Message> {
        let brand = row![text("◉").size(23).color(GREEN), text("ohMyDisk").size(18)]
            .spacing(9)
            .align_y(Alignment::Center);
        container(
            row![
                brand,
                Space::new().width(Length::Fill),
                text(if self.connecting {
                    "正在连接本地服务…"
                } else if self.client.is_some() {
                    "本地磁盘分析"
                } else {
                    "本地服务未连接"
                })
                .size(11)
                .color(MUTED),
                button(text("＋ 扫描文件夹").size(12))
                    .padding([8, 13])
                    .style(accent_button)
                    .on_press_maybe(
                        self.client
                            .as_ref()
                            .filter(|_| self.active.is_none())
                            .map(|_| Message::PickFolder)
                    )
            ]
            .spacing(18)
            .align_y(Alignment::Center),
        )
        .padding([13, 24])
        .into()
    }
    pub(super) fn sources_view(&self) -> Element<'_, Message> {
        let mut disks = column![row![
            column![
                text("磁盘与文件夹").size(27),
                text("先看容量，再探索占用空间。").size(13).color(MUTED)
            ]
            .spacing(8),
            Space::new().width(Length::Fill)
        ]]
        .spacing(18);
        for source in &self.sources {
            let totals = source
                .space_observation
                .as_ref()
                .filter(|o| o.capacity_scope != "caller_view")
                .map(|o| (o.total_bytes, o.available_bytes))
                .unwrap_or((0, 0));
            let valid = totals.0 > 0 && totals.1 <= totals.0;
            let ratio = if valid {
                (totals.0 - totals.1) as f32 / totals.0 as f32
            } else {
                0.0
            };
            let recent = self.scans.iter().find(|scan| {
                scan.root_path.display == source.mount_path.display && scan.root_id.is_some()
            });
            let mut actions = Row::new().spacing(8).align_y(Alignment::Center);
            if let Some(scan) = recent {
                actions = actions.push(
                    button(text("查看结果").size(12))
                        .padding([9, 14])
                        .style(quiet_button)
                        .on_press(Message::OpenScan(scan.scan_id.clone())),
                );
            }
            actions = actions.push(
                button(
                    text(if recent.is_some() {
                        "重新扫描"
                    } else {
                        "扫描"
                    })
                    .size(12),
                )
                .padding([9, 18])
                .style(accent_button)
                .on_press_maybe(
                    source
                        .mount_path
                        .to_path()
                        .ok()
                        .filter(|_| self.active.is_none())
                        .map(Message::StartScan),
                ),
            );
            let info = column![
                text(crop(&source.name, 42)).size(16),
                text(crop(&source.mount_path.display, 75))
                    .size(11)
                    .color(MUTED)
            ]
            .spacing(7);
            let mut usage = column![
                text(if valid {
                    format!("{} 可用 · {} 总容量", bytes(totals.1), bytes(totals.0))
                } else {
                    "容量观测暂不可用".into()
                })
                .size(11)
                .color(MUTED)
            ]
            .spacing(9)
            .width(260);
            if valid {
                usage = usage.push(iced::widget::progress_bar(0.0..=1.0, ratio).girth(5));
            }
            disks = disks.push(
                container(
                    row![
                        text("▣").size(26).color(GREEN),
                        info.width(Length::Fill),
                        usage,
                        actions
                    ]
                    .spacing(25)
                    .align_y(Alignment::Center),
                )
                .padding([20, 22])
                .width(Length::Fill)
                .style(|_| rounded_style(CARD, 12.0)),
            );
            if let Some(hint) = source_hint(source) {
                disks = disks.push(text(hint).size(10).color(MUTED));
            }
        }
        if self.sources.is_empty() {
            disks = disks.push(
                text(if self.connecting {
                    "正在发现磁盘…"
                } else {
                    "选择一个文件夹开始分析。"
                })
                .color(MUTED),
            );
        }
        if cfg!(target_os = "macos")
            && self
                .sources
                .iter()
                .any(|source| source.mount_path.display == "/System/Volumes/Data")
        {
            disks = disks.push(
                text("APFS 系统入口与数据入口可能共享同一容器，容量不能相加。")
                    .size(11)
                    .color(MUTED),
            );
        }
        if self.client.is_none() && !self.connecting {
            disks = disks.push(
                button("重新连接")
                    .style(accent_button)
                    .on_press(Message::Retry),
            );
        }
        disks = disks.push(Space::new().height(14)).push(row![
            text("已保存的扫描").size(16),
            Space::new().width(Length::Fill),
            text(self.scans.len().to_string()).size(12).color(MUTED)
        ]);
        for scan in &self.scans {
            let info = column![
                text(crop(&scan.root_path.display, 92)).size(13),
                text(format!(
                    "{} · {}",
                    scan_size_summary(scan),
                    timestamp(scan.created_at)
                ))
                .size(11)
                .color(MUTED)
            ]
            .spacing(6);
            disks = disks.push(
                row![
                    button(info)
                        .width(Length::Fill)
                        .padding([13, 16])
                        .style(quiet_button)
                        .on_press(Message::OpenScan(scan.scan_id.clone())),
                    button(text("关闭记录").size(11))
                        .padding(10)
                        .style(quiet_button)
                        .on_press(Message::CloseScan(scan.scan_id.clone()))
                ]
                .align_y(Alignment::Center),
            );
        }
        if self.scans.is_empty() {
            disks = disks.push(
                text("扫描完成后，可在这里重新打开结果。")
                    .size(12)
                    .color(MUTED),
            );
        }
        let mut content = column![scrollable(disks).height(Length::Fill)]
            .spacing(15)
            .padding([28, 38])
            .height(Length::Fill);
        if self.active.is_some() {
            content = content.push(self.job_strip());
        }
        content.into()
    }
    pub(super) fn navigation(&self) -> Element<'_, Message> {
        let mut path = Row::new().spacing(3).align_y(Alignment::Center);
        path = path.push(
            button(text("磁盘与文件夹").size(12))
                .padding([7, 8])
                .style(quiet_button)
                .on_press(Message::Overview),
        );
        let skip = self.breadcrumbs.len().saturating_sub(4);
        if skip > 0 {
            path = path.push(text("/ …").size(12).color(MUTED));
        }
        for location in self.breadcrumbs.iter().skip(skip) {
            path = path.push(text("/").size(12).color(MUTED)).push(
                button(text(crop(&location.name, 19)).size(12))
                    .padding([7, 5])
                    .style(quiet_button)
                    .on_press(Message::Navigate(location.id.clone())),
            );
        }
        container(
            row![
                button("‹")
                    .padding([7, 11])
                    .style(quiet_button)
                    .on_press_maybe((self.history_index > 0).then_some(Message::Back)),
                button("›")
                    .padding([7, 11])
                    .style(quiet_button)
                    .on_press_maybe(
                        (self.history_index + 1 < self.history.len()).then_some(Message::Forward)
                    ),
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
            .spacing(5)
            .align_y(Alignment::Center),
        )
        .padding([9, 20])
        .into()
    }
    pub(super) fn job_strip(&self) -> Element<'_, Message> {
        let Some(job) = &self.active else {
            return Space::new().into();
        };
        container(
            row![
                column![
                    text(job_label(&job.work)).size(12).color(GREEN),
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
                        "停止"
                    })
                    .size(11)
                )
                .padding([8, 13])
                .style(quiet_button)
                .on_press_maybe((job.state != "cancel_requested").then_some(Message::CancelJob))
            ]
            .spacing(15)
            .align_y(Alignment::Center),
        )
        .padding([11, 15])
        .style(|_| rounded_style(PANEL, 9.0))
        .into()
    }
    pub(super) fn analysis(&self) -> Element<'_, Message> {
        let Some(scan) = &self.scan else {
            return Space::new().into();
        };
        let attribution_visible = self.capacity().is_some();
        let focus = if attribution_visible {
            self.hover.as_deref().and_then(|id| self.find_node(id))
        } else {
            None
        };
        let root = self.is_root();
        let sector = self.chart_target.as_ref().and_then(|target| {
            self.segments
                .iter()
                .find(|segment| &segment.target == target)
                .filter(|segment| {
                    attribution_visible
                        || (matches!(
                            segment.target,
                            sunburst::Target::Available | sunburst::Target::UsedOverview
                        ) && self
                            .observed_totals()
                            .is_some_and(|(total, _)| segment.bytes <= total))
                })
        });
        let amount =
            if let Some(segment) = sector.filter(|segment| segment.target.node_id().is_none()) {
                bytes(segment.bytes)
            } else if !attribution_visible {
                self.root_amount()
                    .map(bytes)
                    .unwrap_or_else(|| "未测量".into())
            } else if let Some(node) = focus {
                match (node_bytes(node), self.current_node_bytes(node)) {
                    (Some(_), Some(amount)) => bytes(amount),
                    (Some(_), None) => "不一致".into(),
                    (None, _) => "未测量".into(),
                }
            } else if root {
                self.root_amount()
                    .map(bytes)
                    .unwrap_or_else(|| "未测量".into())
            } else {
                self.current_location_bytes()
                    .map(bytes)
                    .unwrap_or_else(|| "未测量".into())
            };
        let name =
            if let Some(segment) = sector.filter(|segment| segment.target.node_id().is_none()) {
                space_title(&segment.target)
            } else if !attribution_visible {
                "所在文件系统"
            } else {
                focus
                    .map(|node| node.name.as_str())
                    .or_else(|| {
                        self.location
                            .as_ref()
                            .map(|location| location.name.as_str())
                    })
                    .unwrap_or("空间分析")
            };
        let center_label = if sector.is_some_and(|segment| segment.target.node_id().is_none()) {
            "空间分类"
        } else if !attribution_visible {
            "所在卷已用/保留"
        } else if focus.is_some_and(|node| node_bytes(node).is_none()) {
            "占用未测量"
        } else if focus.is_some_and(shared_zero) {
            "共享 · 归属占用"
        } else if focus.is_some_and(partly_measured) {
            "至少已归属占用"
        } else if !root
            && self
                .location
                .as_ref()
                .is_some_and(|location| location.measurement_partial)
        {
            "至少已归属占用"
        } else if focus.is_some() || !root {
            "已归属占用"
        } else if self.capacity().is_none() || self.display_mode() == RootMode::Volume {
            "所在卷已用/保留"
        } else if scan
            .space_accounting
            .as_ref()
            .is_some_and(|accounting| accounting.measurement_status == "partial")
        {
            "至少已归属占用"
        } else {
            "已归属占用"
        };
        let center = container(
            column![
                text(center_label).size(10).color(MUTED),
                text(amount).size(26),
                text(crop(name, 10)).size(10).color(MUTED),
                text(if self.breadcrumbs.len() > 1 {
                    "↑ 点击圆心返回"
                } else if focus.is_some() {
                    "文件详情见右侧"
                } else if self.capacity().is_some() && self.display_mode() == RootMode::Directory {
                    "目录内容"
                } else {
                    "系统观测"
                })
                .size(9)
                .color(MUTED)
            ]
            .spacing(6)
            .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill);
        let effective_hover = focus
            .map(|node| sunburst::Target::Node(node.id.clone()))
            .or_else(|| sector.map(|segment| segment.target.clone()));
        let graph = canvas(sunburst::Sunburst {
            segments: &self.segments,
            hover: effective_hover,
            collected: self.chart_collection_ids(),
            can_go_up: self.breadcrumbs.len() > 1,
            drag_label: self
                .drag
                .as_deref()
                .and_then(|id| self.find_node(id))
                .map(|node| node.name.as_str()),
        })
        .width(Length::Fill)
        .height(Length::Fill);
        // The canvas owns pointer hit testing; the central text is a passive overlay.
        let graph = stack![graph, center];
        let status = if scan.known_stale {
            "旧快照 · 正在刷新"
        } else if self.capacity().is_none() {
            "需要重新测量"
        } else if scan
            .space_accounting
            .as_ref()
            .is_some_and(|a| a.measurement_status == "unavailable")
        {
            "未测量"
        } else if scan
            .space_accounting
            .as_ref()
            .is_some_and(|a| a.measurement_status != "complete")
        {
            "部分测量"
        } else if scan.complete {
            "扫描快照"
        } else {
            "扫描不完整"
        };
        let mut chart = column![
            row![
                column![
                    text(crop(
                        self.location
                            .as_ref()
                            .map(|l| l.name.as_str())
                            .unwrap_or("空间分析"),
                        38
                    ))
                    .size(22),
                    text(if self.capacity().is_none() {
                        "当前仅显示系统容量概览"
                    } else if self.loading {
                        "正在载入目录…"
                    } else {
                        "悬停预览 · 点击进入 · 拖入收集区"
                    })
                    .size(11)
                    .color(MUTED)
                ]
                .spacing(7)
                .width(Length::Fill),
                self.scope_switch(),
                pill(
                    status,
                    if self.capacity().is_some() {
                        GREEN
                    } else {
                        WARNING
                    }
                )
            ]
            .align_y(Alignment::Center),
            graph
        ]
        .spacing(12)
        .padding([15, 26])
        .width(Length::Fill)
        .height(Length::Fill);
        if let Some((total, available)) = self.observed_totals() {
            let mut labels = Row::new().spacing(14).align_y(Alignment::Center);
            let directory = self.capacity().is_some() && self.display_mode() == RootMode::Directory;
            if directory {
                labels = labels.push(legend("已归属占用", GREEN)).push(legend(
                    "较小项目与未载入成员",
                    Color::from_rgb(0.27, 0.31, 0.39),
                ));
            } else if self.capacity().is_some() {
                labels = labels
                    .push(legend("已归属", GREEN))
                    .push(legend(
                        "共享、系统及未归属",
                        Color::from_rgb(0.50, 0.50, 0.61),
                    ))
                    .push(legend("可用", MUTED));
            } else {
                labels = labels
                    .push(legend("系统已用 / 保留", Color::from_rgb(0.30, 0.45, 0.66)))
                    .push(legend("可用", MUTED));
            }
            chart = chart.push(
                labels.push(Space::new().width(Length::Fill)).push(
                    text(format!(
                        "{}{} 可用 / {}",
                        if directory { "所在卷 " } else { "" },
                        bytes(available),
                        bytes(total)
                    ))
                    .size(11)
                    .color(MUTED),
                ),
            );
        }
        let captured = if self.capacity().is_some() {
            format!("快照 {}", timestamp(scan.created_at))
        } else if let Some(observation) = self.space_observation() {
            format!("容量观测 {}", timestamp(observation.captured_at))
        } else {
            "未取得系统容量观测".into()
        };
        chart = chart.push(
            text(format!(
                "{} · {captured}",
                self.location
                    .as_ref()
                    .map(|location| crop(&location.path, 75))
                    .unwrap_or_default()
            ))
            .size(10)
            .color(MUTED),
        );
        if self.active.is_some() {
            chart = chart.push(self.job_strip());
        }
        row![chart, self.directory_list()]
            .height(Length::Fill)
            .into()
    }
    pub(super) fn directory_list(&self) -> Element<'_, Message> {
        if self.capacity().is_none() {
            return self.measurement_needed();
        }
        let preview = self
            .preview_owner
            .as_ref()
            .and_then(|id| self.preview_cache.get(id));
        let entries = preview.map(|preview| preview.items.as_slice()).unwrap_or(
            if self.preview_owner.is_some() {
                &[]
            } else {
                &self.children
            },
        );
        let owner = self
            .preview_owner
            .as_deref()
            .and_then(|id| self.find_node(id));
        let heading = owner
            .map(|node| node.name.as_str())
            .or_else(|| {
                self.location
                    .as_ref()
                    .map(|location| location.name.as_str())
            })
            .unwrap_or("目录");
        let related = self.space_info == Some(sunburst::Target::Unattributed);
        let shown_count = entries
            .iter()
            .filter(|node| !related || related_entry(node))
            .count();
        let mut list = Column::new().spacing(2);
        if related {
            list = list.push(text("相关未测量与共享项目").size(11).color(MUTED));
        }
        for (index, node) in entries
            .iter()
            .enumerate()
            .filter(|(_, node)| !related || related_entry(node))
        {
            let hovered = self.hover.as_deref() == Some(&node.id);
            let selected = self.row_is_collected(node);
            let color_index = self
                .segments
                .iter()
                .find(|segment| segment.target.node_id() == Some(node.id.as_str()))
                .map(|segment| segment.color_index)
                .unwrap_or(index);
            let color = sunburst::color(color_index, 0);
            let label = row![
                text(if node.kind == NodeKind::Directory {
                    "●"
                } else {
                    "•"
                })
                .size(11)
                .color(color),
                text(crop(&node.name, 25))
                    .size(12)
                    .color(if selected { MUTED } else { WHITE })
                    .width(Length::Fill),
                text(self.node_amount(node)).size(11).color(MUTED),
                text(if node.kind == NodeKind::Directory {
                    "›"
                } else {
                    ""
                })
                .size(14)
                .color(MUTED)
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            let add: Element<'_, Message> = if hovered || selected {
                button(text(if selected { "✓" } else { "＋" }).size(16))
                    .padding([7, 5])
                    .style(quiet_button)
                    .on_press_maybe(
                        (!selected && node.complete).then(|| Message::Collect(node.id.clone())),
                    )
                    .into()
            } else {
                Space::new().width(25).into()
            };
            list = list.push(
                mouse_area(
                    row![
                        button(label)
                            .width(Length::Fill)
                            .padding([10, 7])
                            .style(move |theme, status| if hovered {
                                selected_button(theme, status)
                            } else {
                                quiet_button(theme, status)
                            })
                            .on_press(Message::Navigate(node.id.clone())),
                        add
                    ]
                    .spacing(0)
                    .align_y(Alignment::Center),
                )
                .on_enter(Message::Hover(Some(node.id.clone())))
                .on_exit(Message::Hover(None)),
            );
        }
        if self.preview_loading {
            list = list.push(text("正在预览目录…").size(12).color(MUTED));
        }
        if let Some(error) = &self.preview_error {
            list = list.push(text(error).size(11).color(WARNING));
        }
        if shown_count == 0 && !self.preview_loading {
            list = list.push(
                text(if self.loading {
                    "正在读取目录…"
                } else if related {
                    "当前已载入列表中没有相关项目。系统开销和扫描范围之外的数据不会作为文件出现。"
                } else if self.preview_owner.is_some() {
                    "此目录没有可列出的子项"
                } else {
                    "此目录没有可列出的子项；未测量和共享项仍会保留在列表中。"
                })
                .size(11)
                .color(MUTED),
            );
        }
        if self.preview_owner.is_some() {
            if preview.is_some_and(|preview| preview.next_cursor.is_some()) {
                list = list.push(text("点击目录，查看所有项目").size(10).color(MUTED));
            }
        } else if self.next_cursor.is_some() {
            list = list.push(
                button(
                    text(if self.loading {
                        "载入中…"
                    } else {
                        "载入更多项目"
                    })
                    .size(11),
                )
                .padding(10)
                .width(Length::Fill)
                .style(quiet_button)
                .on_press_maybe((!self.loading).then_some(Message::MoreChildren)),
            );
        }
        if self.display_mode() == RootMode::Volume && self.preview_owner.is_none() {
            if let Some(capacity) = self.capacity() {
                list = list.push(Space::new().height(12)).push(rule::horizontal(1));
                for (target, label, amount) in [
                    (
                        sunburst::Target::Unattributed,
                        "共享、系统及未归属",
                        capacity.remainder,
                    ),
                    (sunburst::Target::Available, "可用空间", capacity.available),
                ] {
                    list = list.push(
                        button(
                            row![
                                text(label).size(11).color(MUTED).width(Length::Fill),
                                text(bytes(amount)).size(11).color(MUTED)
                            ]
                            .spacing(9),
                        )
                        .padding([11, 7])
                        .width(Length::Fill)
                        .style(quiet_button)
                        .on_press(Message::ShowSpaceInfo(target)),
                    );
                }
            }
        }
        let mut pane = column![
            row![
                text(crop(heading, 30)).size(15),
                Space::new().width(Length::Fill),
                text(if self.preview_owner.is_some() {
                    "预览"
                } else {
                    "按占用排序"
                })
                .size(10)
                .color(MUTED)
            ]
            .spacing(8),
            rule::horizontal(1)
        ];
        if let Some(target) = &self.space_info {
            pane = pane.push(
                container(
                    column![
                        text(space_title(target)).size(13),
                        text(space_description(target)).size(11).color(MUTED),
                        button(text("返回目录").size(10))
                            .padding(5)
                            .style(quiet_button)
                            .on_press(Message::ShowSpaceInfo(target.clone()))
                    ]
                    .spacing(10),
                )
                .padding(13)
                .style(|_| rounded_style(CARD, 9.0)),
            );
        }
        pane = pane.push(scrollable(list).height(Length::Fill));
        if let Some(node) = self.hover.as_deref().and_then(|id| self.find_node(id)) {
            pane = pane
                .push(rule::horizontal(1))
                .push(text(crop(&node.path.display, 90)).size(10).color(MUTED))
                .push(
                    text(format!(
                        "详情 · 逻辑大小 {} · 报告分配 {}",
                        bytes(node.logical_bytes),
                        node.allocated_bytes
                            .map(bytes)
                            .unwrap_or_else(|| "未知".into())
                    ))
                    .size(10)
                    .color(MUTED),
                );
            if shared_zero(node) {
                pane = pane.push(
                    text("这个项目仍包含数据；共享数据未独占归属到此项。")
                        .size(10)
                        .color(MUTED),
                );
            }
            pane = pane.push(
                button(text("在文件管理器中显示").size(11))
                    .padding(7)
                    .style(quiet_button)
                    .on_press_maybe(self.current_reveal_message(&node.id)),
            );
        }
        mouse_area(
            container(pane.spacing(12))
                .padding([20, 17])
                .width(350)
                .height(Length::Fill)
                .style(|_| panel_style(PANEL)),
        )
        .on_enter(Message::PreviewPane(true))
        .on_exit(Message::PreviewPane(false))
        .into()
    }
    pub(super) fn measurement_needed(&self) -> Element<'_, Message> {
        let inconsistent = self
            .scan
            .as_ref()
            .and_then(|scan| scan.space_accounting.as_ref())
            .is_some();
        container(
            column![
                text(if inconsistent {
                    "占用测量未对齐"
                } else {
                    "需要重新测量占用"
                })
                .size(17),
                text(if inconsistent {
                    "系统容量、可用空间与扫描归属暂不能组成同一份占用图。当前只显示系统容量概览。"
                } else {
                    "这份扫描没有占用测量。重新扫描后，可按实际归属探索文件与目录。"
                })
                .size(12)
                .color(MUTED),
                text("原始逻辑大小仍保留，不会作为占用空间显示。")
                    .size(11)
                    .color(MUTED),
                button(text("重新扫描").size(12))
                    .padding([10, 16])
                    .style(accent_button)
                    .on_press_maybe(self.active.is_none().then_some(Message::Rescan))
            ]
            .spacing(17),
        )
        .padding(24)
        .width(350)
        .height(Length::Fill)
        .style(|_| panel_style(PANEL))
        .into()
    }
    pub(super) fn collector(&self) -> Element<'_, Message> {
        let drag_name = self
            .drag
            .as_deref()
            .and_then(|id| self.find_node(id))
            .map(|node| node.name.as_str());
        let summary = if let Some(name) = drag_name {
            format!("释放以收集「{}」", crop(name, 38))
        } else if self.collection.is_empty() {
            "拖入项目，或在目录旁点 ＋".into()
        } else {
            let amount = self.collection_amount();
            if self.collection_context() != CollectionContext::Current {
                format!("{} 项 · {amount}", self.collection.len())
            } else if amount == "未测量" {
                format!("{} 项 · 占用未测量", self.collection.len())
            } else {
                format!("{} 项 · 已知归属 {}", self.collection.len(), amount)
            }
        };
        let enabled = !self.collection.is_empty() && self.active.is_none() && !self.busy;
        let title = row![
            button(
                text(if self.collector_expanded {
                    "⌄ 收集区"
                } else {
                    "› 收集区"
                })
                .size(13)
            )
            .padding([8, 4])
            .style(quiet_button)
            .on_press(Message::ToggleCollector),
            text(summary)
                .size(12)
                .color(if drag_name.is_some() { GREEN } else { MUTED }),
            Space::new().width(Length::Fill),
            button(text("清空").size(11))
                .padding([8, 10])
                .style(quiet_button)
                .on_press_maybe(
                    (!self.collection.is_empty() && !self.busy).then_some(Message::ClearCollection)
                ),
            button(
                text(if self.busy {
                    "正在准备…"
                } else {
                    "移入回收站…"
                })
                .size(12)
            )
            .padding([9, 14])
            .style(accent_button)
            .on_press_maybe(enabled.then_some(Message::Prepare(CleanupMode::Trash)))
        ]
        .spacing(13)
        .align_y(Alignment::Center);
        let mut body = column![title].spacing(12);
        if self.collector_expanded {
            let mut chips = Row::new().spacing(10);
            for node in &self.collection {
                chips = chips.push(
                    container(
                        row![
                            column![
                                text(crop(&node.name, 24)).size(12),
                                text(self.collection_node_amount(node))
                                    .size(10)
                                    .color(MUTED)
                            ]
                            .spacing(5),
                            button("×")
                                .padding(5)
                                .style(quiet_button)
                                .on_press(Message::Remove(node.id.clone()))
                        ]
                        .spacing(14)
                        .align_y(Alignment::Center),
                    )
                    .padding([10, 13])
                    .style(|_| rounded_style(CARD, 8.0)),
                );
            }
            if self.collection.is_empty() {
                chips = chips.push(
                    text("收集只保存选择，执行前会逐项核对。")
                        .size(11)
                        .color(MUTED),
                );
            }
            body = body
                .push(
                    row![
                        scrollable(chips)
                            .direction(scrollable::Direction::Horizontal(
                                scrollable::Scrollbar::default()
                            ))
                            .width(Length::Fill),
                        button(text("永久删除…").size(11))
                            .padding([9, 13])
                            .style(quiet_button)
                            .on_press_maybe(
                                enabled.then_some(Message::Prepare(CleanupMode::Permanent))
                            )
                    ]
                    .spacing(20)
                    .align_y(Alignment::Center),
                )
                .push(
                    text("收集大小不等于可释放空间；实际变化以清理后的系统观测为准。")
                        .size(10)
                        .color(MUTED),
                );
        }
        mouse_area(
            container(body)
                .padding([10, 24])
                .width(Length::Fill)
                .style(move |_| {
                    let mut style = panel_style(if self.collector_hover && self.drag.is_some() {
                        Color::from_rgb(0.10, 0.22, 0.22)
                    } else {
                        PANEL
                    });
                    style.border = Border {
                        color: if self.collector_hover && self.drag.is_some() {
                            GREEN
                        } else {
                            Color::from_rgb(0.16, 0.19, 0.25)
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
}
fn related_entry(node: &Node) -> bool {
    node_bytes(node).is_none() || partly_measured(node) || shared_zero(node)
}
fn legend(label: &str, color: Color) -> Element<'_, Message> {
    row![
        text("●").size(8).color(color),
        text(label).size(10).color(MUTED)
    ]
    .spacing(5)
    .align_y(Alignment::Center)
    .into()
}
fn space_title(target: &sunburst::Target) -> &'static str {
    match target {
        sunburst::Target::Available => "可用空间",
        sunburst::Target::Unattributed => "共享、系统及未归属空间",
        sunburst::Target::UsedOverview => "系统已用 / 保留空间",
        _ => "较小项目与未载入成员",
    }
}
fn space_description(target: &sunburst::Target) -> &'static str {
    match target {
        sunburst::Target::Available => {
            "这是系统报告的原始可用容量。它属于磁盘容量的一部分，无法加入清理计划。"
        }
        sunburst::Target::Unattributed => {
            "这部分尚未独占归属到扫描内文件，可能包括共享数据、快照、系统开销、权限受限项目，以及扫描范围之外的文件。它不是一个可删除的目录。"
        }
        sunburst::Target::UsedOverview => {
            "这是容量减去原始可用空间的结果。完成占用测量后，才能进一步分解到目录。"
        }
        _ => "这些成员在当前图中合并显示，打开对应目录后可在列表中逐项查看。",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ohmydisk_core::{NativePath, NodeSpace};
    fn node(id: &str, bytes: Option<u64>) -> Node {
        Node {
            id: id.into(),
            parent_id: Some("root".into()),
            name: id.into(),
            path: NativePath::new(std::path::Path::new("/tmp/fixture")),
            kind: NodeKind::Directory,
            logical_bytes: 1_100,
            allocated_bytes: Some(80),
            space_usage: bytes.map(|attributed_bytes| NodeSpace {
                attributed_bytes,
                measured_entries: 1,
                unmeasured_entries: 0,
            }),
            modified_ns: None,
            identity: id.into(),
            volume_id: "fixture".into(),
            link_count: 1,
            complete: true,
            is_link_or_reparse: false,
        }
    }
    fn app() -> App {
        App::new(PathBuf::from("/tmp/ohmydisk-gui-unit-test-unused")).0
    }
    fn measured_app(root: &str, status: &str) -> App {
        let mut app = app();
        app.sources = vec![Volume {
            id: "volume".into(),
            name: "test volume".into(),
            mount_path: NativePath::new(std::path::Path::new("/")),
            total_bytes: 995_000_000_000,
            available_bytes: 245_000_000_000,
            space_observation: None,
        }];
        let observation = json!({"volume_id":"volume","total_bytes":995_000_000_000_u64,"available_bytes":245_000_000_000_u64,
            "captured_at":0,"source":"fixture","capacity_scope":"volume","provider":"mac_private_size"});
        let attributed = if status == "unavailable" {
            0
        } else {
            8_800_000_u64
        };
        let value = json!({"scan_id":"measured","root_id":"root","root_path":NativePath::new(std::path::Path::new(root)),"complete":true,
            "space_accounting":{"before":observation,"after":observation,"attributed_bytes":attributed,
                "shared_unattributed_bytes":750_000_000_000_u64-attributed,"reconciliation_status":"balanced","measurement_status":status}});
        let _ = app.receive(Response::Scan { epoch: 0 }, value);
        app.children = vec![node("branch", Some(attributed))];
        app.rebuild_chart();
        app
    }
    fn ring_total(app: &App) -> u64 {
        app.segments
            .iter()
            .filter(|segment| segment.depth == 0)
            .map(|segment| segment.bytes)
            .sum()
    }
    #[test]
    fn small_folder_defaults_to_its_attribution_and_toggle_preserves_each_scope() {
        let mut app = measured_app("/tmp/fixture", "complete");
        assert_eq!(app.display_mode(), RootMode::Directory);
        assert_eq!(app.root_amount(), Some(8_800_000));
        assert_eq!(ring_total(&app), 8_800_000);
        assert!(!app.segments.iter().any(|segment| matches!(
            segment.target,
            sunburst::Target::Available | sunburst::Target::Unattributed
        )));
        let _ = app.update(Message::SetRootMode(RootMode::Volume));
        assert_eq!(app.root_amount(), Some(750_000_000_000));
        assert_eq!(ring_total(&app), 995_000_000_000);
        assert_eq!(
            app.segments
                .iter()
                .filter(|segment| segment.depth == 0
                    && matches!(
                        segment.target,
                        sunburst::Target::Node(_) | sunburst::Target::Aggregate { .. }
                    ))
                .map(|segment| segment.bytes)
                .sum::<u64>(),
            8_800_000
        );
        let _ = app.navigate("branch", true);
        app.rebuild_chart();
        assert_eq!(app.display_mode(), RootMode::Directory);
        assert_eq!(ring_total(&app), 8_800_000);
        let _ = app.update(Message::SetRootMode(RootMode::Directory));
        assert_eq!(app.root_mode, RootMode::Volume); // A child cannot change the root's preference.
        let _ = app.update(Message::Up);
        app.rebuild_chart();
        assert_eq!(app.display_mode(), RootMode::Volume);
        assert_eq!(ring_total(&app), 995_000_000_000);
    }
    #[test]
    fn mounted_root_defaults_to_volume_and_unknown_attribution_is_not_zero() {
        let app = measured_app("/", "complete");
        assert_eq!(app.root_mode, RootMode::Volume);
        assert_eq!(app.root_amount(), Some(750_000_000_000));
        let unknown = measured_app("/tmp/fixture", "unavailable");
        assert_eq!(unknown.root_amount(), None);
        assert!(unknown.segments.is_empty());
        let partial = measured_app("/tmp/fixture", "partial");
        assert_eq!(partial.root_amount(), Some(8_800_000));
        assert!(partial.location.as_ref().unwrap().measurement_partial);
    }
    #[test]
    fn an_invalid_or_legacy_ledger_cannot_enable_a_root_scope_toggle() {
        let mut app = measured_app("/tmp/fixture", "complete");
        app.scan
            .as_mut()
            .unwrap()
            .space_accounting
            .as_mut()
            .unwrap()
            .reconciliation_status = "inconsistent".into();
        let _ = app.update(Message::SetRootMode(RootMode::Volume));
        assert_eq!(app.root_mode, RootMode::Directory);
        app.scan.as_mut().unwrap().space_accounting = None;
        let _ = app.update(Message::SetRootMode(RootMode::Volume));
        assert_eq!(app.root_mode, RootMode::Directory);
    }
    #[test]
    fn unknown_physical_size_does_not_block_a_complete_real_entry_collection() {
        let mut app = measured_app("/tmp/fixture", "unavailable");
        let mut unknown = node("unknown-complete", None);
        unknown.kind = NodeKind::File;
        app.children = vec![unknown];
        app.collect("unknown-complete");
        assert_eq!(app.collection.len(), 1);
        assert_eq!(app.collection_amount(), "未测量");
        let mut incomplete = node("incomplete", None);
        incomplete.complete = false;
        incomplete.kind = NodeKind::File;
        app.children.push(incomplete);
        app.collect("incomplete");
        assert_eq!(app.collection.len(), 1);
        assert!(app.banner.as_ref().unwrap().contains("不完整"));
    }
    #[test]
    fn selection_styles_do_not_cross_scan_ids_or_reused_paths() {
        let mut app = measured_app("/tmp/fixture", "complete");
        let retained = node("reused-id", Some(100));
        app.collection = vec![retained.clone()];
        app.draft_scan_id = Some("scan-A".into());
        assert!(app.chart_collection_ids().is_empty());
        assert!(!app.row_is_collected(&retained));
        assert!(!app.row_is_collected(&node("different-id-same-path", Some(100))));
        assert_eq!(app.collection.len(), 1);
        app.draft_scan_id = Some("measured".into());
        assert_eq!(app.chart_collection_ids(), vec!["reused-id".to_owned()]);
        assert!(app.row_is_collected(&retained));
        app.scan
            .as_mut()
            .unwrap()
            .space_accounting
            .as_mut()
            .unwrap()
            .reconciliation_status = "inconsistent".into();
        assert!(app.chart_collection_ids().is_empty());
        assert!(!app.row_is_collected(&retained));
        assert_eq!(app.collection.len(), 1);
    }
    #[test]
    fn reveal_event_keeps_the_displayed_scan_when_current_scan_changes() {
        let mut app = measured_app("/tmp/fixture", "complete");
        let message = app.current_reveal_message("reused-id").unwrap();
        app.scan.as_mut().unwrap().scan_id = "scan-B".into();
        match message {
            Message::RevealInScan { scan_id, node_id } => {
                assert_eq!(scan_id, "measured");
                assert_eq!(node_id, "reused-id");
                assert_ne!(scan_id, app.scan.as_ref().unwrap().scan_id);
            }
            _ => panic!("Reveal must carry its originating scan"),
        }
    }
    #[test]
    fn inconsistent_current_ledger_hides_collection_values_even_below_capacity() {
        let mut app = measured_app("/tmp/fixture", "complete");
        let selected = node("selected", Some(800_000_000_000));
        app.collection = vec![selected.clone()];
        app.draft_scan_id = Some("measured".into());
        let accounting = app
            .scan
            .as_mut()
            .unwrap()
            .space_accounting
            .as_mut()
            .unwrap();
        accounting.attributed_bytes = 800_000_000_000;
        accounting.shared_unattributed_bytes = None;
        accounting.reconciliation_status = "inconsistent".into();
        assert!(app.collection_amount().contains("待复核"));
        assert!(app.collection_node_amount(&selected).contains("待复核"));
        assert!(app.node_amount(&selected).contains("待复核"));
        assert_eq!(app.root_amount(), Some(750_000_000_000));
        assert_eq!(app.current_node_bytes(&selected), None);
        assert_eq!(app.collection.len(), 1);
    }
    #[test]
    fn collection_from_another_scan_is_not_judged_against_the_current_volume() {
        let mut app = measured_app("/tmp/fixture", "complete");
        let selected = node("from-larger-volume", Some(1_100_000_000_000));
        app.collection = vec![selected.clone()];
        app.draft_scan_id = Some("scan-A".into());
        assert!(app.collection_amount().contains("其他扫描"));
        assert!(app.collection_node_amount(&selected).contains("其他扫描"));
        assert!(!app.collection_amount().contains("测量不一致"));
        app.preview_cache.insert(
            "current-preview".into(),
            Preview {
                items: vec![node("from-larger-volume", Some(2_000))],
                next_cursor: None,
            },
        );
        assert_eq!(
            node_bytes(app.find_node("from-larger-volume").unwrap()),
            Some(2_000)
        );
        let _ = app.update(Message::Prepare(CleanupMode::Trash));
        assert!(app.busy); // Presentation status does not disable preparing the original draft.
        assert_eq!(app.draft_scan_id.as_deref(), Some("scan-A"));
        assert_eq!(app.collection.len(), 1);
    }
    #[test]
    fn missing_valid_capacity_hides_stale_focus_and_location_without_zeroing_metadata() {
        let mut app = measured_app("/tmp/fixture", "complete");
        let mut selected = node("stale", Some(1_100_000_000_000));
        selected.logical_bytes = 1_100_000_000_000;
        app.collection = vec![selected.clone()];
        app.draft_scan_id = Some("measured".into());
        app.location.as_mut().unwrap().bytes = Some(1_100_000_000_000);
        app.hover = Some("stale".into());
        let accounting = app
            .scan
            .as_mut()
            .unwrap()
            .space_accounting
            .as_mut()
            .unwrap();
        accounting.after.as_mut().unwrap().available_bytes = 996_000_000_000;
        accounting.reconciliation_status = "inconsistent".into();
        assert_eq!(app.observed_totals(), None);
        assert_eq!(app.root_amount(), None);
        assert_eq!(app.current_node_bytes(&selected), None);
        assert_eq!(app.current_location_bytes(), None);
        assert!(app.collection_amount().contains("待复核"));
        assert!(app.collection_node_amount(&selected).contains("待复核"));
        assert_eq!(
            app.location.as_ref().unwrap().bytes,
            Some(1_100_000_000_000)
        );
        assert_eq!(app.collection[0].logical_bytes, 1_100_000_000_000);
        assert_eq!(node_bytes(&app.collection[0]), Some(1_100_000_000_000));
    }
    #[test]
    fn legacy_compatibility_capacity_fields_are_not_physical_observations() {
        let mut app = app();
        app.scan = Some(serde_json::from_value(json!({"scan_id":"legacy","root_id":"root",
            "root_path":NativePath::new(std::path::Path::new("/tmp/fixture")),"logical_bytes":1_100})).unwrap());
        app.sources = vec![serde_json::from_value(json!({"id":"compat","name":"disk",
            "mount_path":NativePath::new(std::path::Path::new("/")),"total_bytes":995,"available_bytes":245})).unwrap()];
        assert!(app.observed_totals().is_none());
        assert!(app.capacity().is_none());
    }
    #[test]
    fn opening_a_scan_replaces_breadcrumbs_even_when_root_ids_match() {
        let mut app = app();
        app.epoch = 1;
        let previous = Location {
            id: "root".into(),
            name: "old scan".into(),
            path: "/tmp/old".into(),
            bytes: None,
            measurement_partial: false,
        };
        app.breadcrumbs = vec![previous.clone()];
        app.history = vec![previous];
        let _ = app.receive(
            Response::Scan { epoch: 1 },
            json!({"scan_id":"new-scan","root_id":"root",
            "root_path":NativePath::new(std::path::Path::new("/tmp/new-scan"))}),
        );
        assert_eq!(app.breadcrumbs.len(), 1);
        assert_eq!(app.breadcrumbs[0].name, "new-scan");
    }
    #[test]
    fn stale_hover_responses_cannot_replace_current_preview() {
        let mut app = app();
        app.epoch = 4;
        app.preview_epoch = 9;
        app.preview_owner = Some("current".into());
        app.preview_loading = true;
        let _ = app.receive(
            Response::Preview {
                scan_epoch: 4,
                hover_epoch: 8,
                owner: "old".into(),
            },
            json!({"items":[node("old-child",Some(3))]}),
        );
        assert_eq!(app.preview_owner.as_deref(), Some("current"));
        assert!(app.preview_loading);
        let _ = app.receive(
            Response::Preview {
                scan_epoch: 3,
                hover_epoch: 9,
                owner: "current".into(),
            },
            json!({"items":[node("wrong-scan",Some(9))]}),
        );
        assert!(!app.preview_cache.contains_key("current"));
        let _ = app.receive(
            Response::Preview {
                scan_epoch: 4,
                hover_epoch: 9,
                owner: "current".into(),
            },
            json!({"items":[node("correct",Some(8))]}),
        );
        assert!(!app.preview_loading);
        assert_eq!(app.preview_cache["current"].items[0].id, "correct");
    }
    #[test]
    fn leaving_chart_preserves_preview_for_panel_entry_and_old_clear_is_ignored() {
        let mut app = app();
        app.children = vec![node("folder", Some(5))];
        let _ = app.chart_hover(Some(sunburst::Target::Node("folder".into())));
        let epoch = app.preview_epoch;
        let _ = app.chart_hover(None);
        assert_eq!(app.preview_epoch, epoch);
        let _ = app.update(Message::PreviewPane(true));
        let _ = app.update(Message::ClearPreview { epoch });
        assert_eq!(app.preview_owner.as_deref(), Some("folder"));
        let _ = app.chart_hover(Some(sunburst::Target::Available));
        let newer = app.preview_epoch;
        let _ = app.update(Message::ClearPreview { epoch });
        assert_eq!(app.preview_epoch, newer);
        assert_eq!(app.chart_target, Some(sunburst::Target::Available));
    }
    #[test]
    fn unknown_and_shared_zero_are_not_reported_as_empty_directories() {
        let app = measured_app("/tmp/fixture", "complete");
        assert_eq!(app.node_amount(&node("unknown", None)), "未测量");
        assert_eq!(app.node_amount(&node("shared", Some(0))), "共享 · 归属 0 B");
        let mut partial = node("partial", Some(7));
        partial.space_usage.as_mut().unwrap().unmeasured_entries = 1;
        assert!(app.node_amount(&partial).contains("部分"));
    }
}
