use std::env;
use std::f32::consts::{PI, TAU};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::{App, CreationContext, NativeOptions, egui};
use egui::{Align2, Color32, FontId, Pos2, Rect, Shape, Stroke, Vec2};
use parking_lot::{Condvar, Mutex, RwLock};

const WINDOW_SIZE: [f32; 2] = [980.0, 760.0];
const CAPACITY: usize = 8;
const MAX_ACTORS: usize = 8;
const START_STAGGER_MS: u64 = 110;
const THINK_MIN_MS: u64 = 600;
const THINK_RANGE_MS: u64 = 1800;
const ITEM_ANIMATION_MS: u64 = 700;
const QUEUE_CENTER_X: f32 = 490.0;
const QUEUE_CENTER_Y: f32 = 275.0;
const INNER_RADIUS: f32 = 78.0;
const OUTER_RADIUS: f32 = 150.0;
const SLOT_RADIUS: f32 = 108.0;
const PRODUCER_X: f32 = 150.0;
const CONSUMER_X: f32 = 830.0;
const ACTOR_START_Y: f32 = 110.0;
const ACTOR_Y_STEP: f32 = 60.0;

fn main() -> eframe::Result<()> {
    let config = ProducerConsumerConfig::from_args();
    let title = format!(
        "Producer-Consumer ({} producers, {} consumers)",
        config.producer_count, config.consumer_count
    );

    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([900.0, 700.0]),
        ..Default::default()
    };

    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| Box::new(ProducerConsumerApp::new(cc, config.clone()))),
    )
}

#[derive(Clone, Debug)]
struct ProducerConsumerConfig {
    producer_count: usize,
    consumer_count: usize,
}

impl ProducerConsumerConfig {
    fn from_args() -> Self {
        let args: Vec<String> = env::args().skip(1).collect();
        let default = Self {
            producer_count: 5,
            consumer_count: 5,
        };
        if args.is_empty() {
            return default;
        }

        let parsed_producers = args.first().and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
        let parsed_consumers = args
            .get(1)
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(5);

        if parsed_producers <= 0
            || parsed_consumers <= 0
            || parsed_producers as usize > MAX_ACTORS
            || parsed_consumers as usize > MAX_ACTORS
        {
            return Self {
                producer_count: MAX_ACTORS,
                consumer_count: MAX_ACTORS,
            };
        }

        Self {
            producer_count: parsed_producers as usize,
            consumer_count: parsed_consumers as usize,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActorKind {
    Producer,
    Consumer,
}

impl ActorKind {
    fn label(self) -> &'static str {
        match self {
            Self::Producer => "Producer",
            Self::Consumer => "Consumer",
        }
    }

    fn active_label(self) -> &'static str {
        match self {
            Self::Producer => "Producing",
            Self::Consumer => "Consuming",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActorState {
    ActiveColor,
    Waiting,
    HoldingLock,
    Stopped,
}

impl ActorState {
    fn label(self, kind: ActorKind) -> &'static str {
        match self {
            Self::ActiveColor => kind.active_label(),
            Self::Waiting => "Waiting for lock",
            Self::HoldingLock => "Holding lock",
            Self::Stopped => "Stopped",
        }
    }
}

#[derive(Clone, Debug)]
struct QueueItem {
    id: u64,
    color: Color32,
    producer_id: usize,
}

#[derive(Clone, Debug)]
struct QueueSlot {
    item: Option<QueueItem>,
}

#[derive(Clone, Copy, Debug, Default)]
struct QueueStats {
    count: usize,
    waiting_producers: usize,
    waiting_consumers: usize,
}

#[derive(Clone, Debug)]
struct ActorVisual {
    kind: ActorKind,
    id: usize,
    count: usize,
    color: Color32,
    state: ActorState,
}

#[derive(Clone, Debug)]
struct MovingItemVisual {
    id: u64,
    color: Color32,
    position: Pos2,
}

#[derive(Debug)]
struct AppState {
    queue_slots: Vec<QueueSlot>,
    queue_stats: QueueStats,
    producers: Vec<ActorVisual>,
    consumers: Vec<ActorVisual>,
    moving_items: Vec<MovingItemVisual>,
}

impl AppState {
    fn new(config: &ProducerConsumerConfig) -> Self {
        let producers = (0..config.producer_count)
            .map(|id| ActorVisual {
                kind: ActorKind::Producer,
                id,
                count: 0,
                color: Color32::BLACK,
                state: ActorState::Waiting,
            })
            .collect();
        let consumers = (0..config.consumer_count)
            .map(|id| ActorVisual {
                kind: ActorKind::Consumer,
                id,
                count: 0,
                color: Color32::BLACK,
                state: ActorState::Waiting,
            })
            .collect();

        Self {
            queue_slots: vec![QueueSlot { item: None }; CAPACITY],
            queue_stats: QueueStats::default(),
            producers,
            consumers,
            moving_items: Vec::new(),
        }
    }

    fn actor_mut(&mut self, kind: ActorKind, id: usize) -> &mut ActorVisual {
        match kind {
            ActorKind::Producer => &mut self.producers[id],
            ActorKind::Consumer => &mut self.consumers[id],
        }
    }

    fn set_actor(&mut self, kind: ActorKind, id: usize, state: ActorState, color: Color32) {
        let actor = self.actor_mut(kind, id);
        actor.state = state;
        actor.color = color;
    }

    fn increment_actor_count(&mut self, kind: ActorKind, id: usize, color: Color32) {
        let actor = self.actor_mut(kind, id);
        actor.count += 1;
        actor.color = color;
        actor.state = ActorState::ActiveColor;
    }

    fn set_queue_snapshot(&mut self, snapshot: &QueueSnapshot) {
        self.queue_slots = snapshot
            .slots
            .iter()
            .cloned()
            .map(|item| QueueSlot { item })
            .collect();
        self.queue_stats = QueueStats {
            count: snapshot.count,
            waiting_producers: snapshot.waiting_producers,
            waiting_consumers: snapshot.waiting_consumers,
        };
    }

    fn add_moving_item(&mut self, id: u64, color: Color32, position: Pos2) {
        self.moving_items.push(MovingItemVisual {
            id,
            color,
            position,
        });
    }

    fn move_item(&mut self, id: u64, position: Pos2) {
        if let Some(item) = self.moving_items.iter_mut().find(|item| item.id == id) {
            item.position = position;
        }
    }

    fn remove_moving_item(&mut self, id: u64) {
        self.moving_items.retain(|item| item.id != id);
    }
}

struct ProducerConsumerApp {
    config: ProducerConsumerConfig,
    shared: Arc<RwLock<AppState>>,
    runtime: Option<Runtime>,
}

impl ProducerConsumerApp {
    fn new(_cc: &CreationContext<'_>, config: ProducerConsumerConfig) -> Self {
        let (shared, runtime) = Self::build_runtime(&config);
        Self {
            config,
            shared,
            runtime: Some(runtime),
        }
    }

    fn build_runtime(config: &ProducerConsumerConfig) -> (Arc<RwLock<AppState>>, Runtime) {
        let shared = Arc::new(RwLock::new(AppState::new(config)));
        let queue = Arc::new(BoundedQueue::new(CAPACITY));
        {
            let snapshot = queue.snapshot();
            shared.write().set_queue_snapshot(&snapshot);
        }
        let runtime = Runtime::spawn(config.clone(), Arc::clone(&shared), queue);
        (shared, runtime)
    }

    fn reset_runtime(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.stop();
        }
        let (shared, runtime) = Self::build_runtime(&self.config);
        self.shared = shared;
        self.runtime = Some(runtime);
    }

    fn draw_scene(&self, ui: &mut egui::Ui) {
        let painter = ui.painter();
        let app = self.shared.read();
        painter.rect_filled(ui.max_rect(), 0.0, Color32::from_rgb(249, 249, 246));

        painter.text(
            Pos2::new(WINDOW_SIZE[0] * 0.5, 44.0),
            Align2::CENTER_CENTER,
            "Producer-Consumer",
            FontId::proportional(30.0),
            Color32::BLACK,
        );
        painter.text(
            Pos2::new(PRODUCER_X, 72.0),
            Align2::CENTER_CENTER,
            "Producers",
            FontId::proportional(24.0),
            Color32::BLACK,
        );
        painter.text(
            Pos2::new(CONSUMER_X, 72.0),
            Align2::CENTER_CENTER,
            "Consumers",
            FontId::proportional(24.0),
            Color32::BLACK,
        );

        draw_queue_frame(painter);

        for (slot_idx, slot) in app.queue_slots.iter().enumerate() {
            if let Some(item) = &slot.item {
                let center = queue_slot_center(slot_idx);
                draw_star(
                    painter,
                    center,
                    18.0,
                    9.0,
                    5,
                    item.color,
                    Stroke::new(2.0, Colors::producer_palette(item.producer_id)),
                );
            }
        }

        for item in &app.moving_items {
            draw_star(
                painter,
                item.position,
                18.0,
                9.0,
                5,
                item.color,
                Stroke::new(1.5, Color32::BLACK),
            );
        }

        for actor in &app.producers {
            draw_actor(painter, actor, producer_center(actor.id), true);
        }
        for actor in &app.consumers {
            draw_actor(painter, actor, consumer_center(actor.id), false);
        }

        painter.text(
            Pos2::new(WINDOW_SIZE[0] * 0.5, 470.0),
            Align2::CENTER_CENTER,
            format!(
                "Queue count: {}   Waiting producers: {}   Waiting consumers: {}",
                app.queue_stats.count, app.queue_stats.waiting_producers, app.queue_stats.waiting_consumers
            ),
            FontId::proportional(18.0),
            Color32::from_rgb(40, 40, 40),
        );

        painter.text(
            Pos2::new(WINDOW_SIZE[0] * 0.5, WINDOW_SIZE[1] - 36.0),
            Align2::CENTER_CENTER,
            "*Numbers indicate counts of items produced and consumed",
            FontId::proportional(15.0),
            Color32::from_rgb(55, 55, 55),
        );

        draw_legend(painter);
    }
}

impl App for ProducerConsumerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));
        let mut should_reset = false;
        let space_pressed = ctx.input(|input| input.key_pressed(egui::Key::Space));

        if space_pressed && let Some(runtime) = self.runtime.as_ref() {
            let paused = runtime.is_paused();
            runtime.set_paused(!paused);
        }

        egui::TopBottomPanel::top("controls").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let paused = self
                    .runtime
                    .as_ref()
                    .map(|runtime| runtime.is_paused())
                    .unwrap_or(false);
                let pause_label = if paused { "Resume" } else { "Pause" };
                if ui.button(pause_label).clicked()
                    && let Some(runtime) = self.runtime.as_ref()
                {
                    runtime.set_paused(!paused);
                }
                if ui.button("Reset").clicked() {
                    should_reset = true;
                }
                ui.separator();
                ui.label(format!(
                    "{} producers, {} consumers",
                    self.config.producer_count, self.config.consumer_count
                ));
                ui.separator();
                ui.label("Space: pause/resume");
            });
        });

        if should_reset {
            self.reset_runtime();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_scene(ui);
        });
    }
}

impl Drop for ProducerConsumerApp {
    fn drop(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.stop();
        }
    }
}

struct Runtime {
    running: Arc<AtomicBool>,
    control: Arc<RunControl>,
    queue: Arc<BoundedQueue>,
    handles: Vec<JoinHandle<()>>,
}

impl Runtime {
    fn spawn(
        config: ProducerConsumerConfig,
        shared: Arc<RwLock<AppState>>,
        queue: Arc<BoundedQueue>,
    ) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let control = Arc::new(RunControl::new());
        let mut handles = Vec::new();

        for id in 0..config.producer_count {
            let shared = Arc::clone(&shared);
            let queue = Arc::clone(&queue);
            let running = Arc::clone(&running);
            let control = Arc::clone(&control);
            handles.push(thread::spawn(move || {
                thread::sleep(Duration::from_millis(id as u64 * START_STAGGER_MS));
                producer_worker(id, shared, queue, running, control);
            }));
        }

        for id in 0..config.consumer_count {
            let shared = Arc::clone(&shared);
            let queue = Arc::clone(&queue);
            let running = Arc::clone(&running);
            let control = Arc::clone(&control);
            let producer_count = config.producer_count;
            handles.push(thread::spawn(move || {
                thread::sleep(Duration::from_millis(
                    producer_count as u64 * START_STAGGER_MS + (id as u64 + 1) * START_STAGGER_MS,
                ));
                consumer_worker(id, shared, queue, running, control);
            }));
        }

        Self {
            running,
            control,
            queue,
            handles,
        }
    }

    fn is_paused(&self) -> bool {
        self.control.is_paused()
    }

    fn set_paused(&self, paused: bool) {
        self.control.set_paused(paused);
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.control.wake_all();
        self.queue.wake_all();
        while let Some(handle) = self.handles.pop() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
struct RunControl {
    state: Mutex<ControlState>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct ControlState {
    paused: bool,
}

impl RunControl {
    fn new() -> Self {
        Self {
            state: Mutex::new(ControlState::default()),
            cv: Condvar::new(),
        }
    }

    fn is_paused(&self) -> bool {
        self.state.lock().paused
    }

    fn set_paused(&self, paused: bool) {
        self.state.lock().paused = paused;
        if !paused {
            self.cv.notify_all();
        }
    }

    fn wait_if_paused(&self, running: &AtomicBool) -> bool {
        let mut guard = self.state.lock();
        while guard.paused && running.load(Ordering::Relaxed) {
            self.cv.wait_for(&mut guard, Duration::from_millis(100));
        }
        running.load(Ordering::Relaxed)
    }

    fn wake_all(&self) {
        self.cv.notify_all();
    }
}

#[derive(Clone, Debug)]
struct QueueSnapshot {
    slots: Vec<Option<QueueItem>>,
    count: usize,
    waiting_producers: usize,
    waiting_consumers: usize,
}

#[derive(Debug)]
struct BoundedQueue {
    state: Mutex<QueueState>,
    not_empty: Condvar,
    not_full: Condvar,
}

#[derive(Debug)]
struct QueueState {
    slots: Vec<Option<QueueItem>>,
    first: usize,
    last: usize,
    count: usize,
    waiting_producers: usize,
    waiting_consumers: usize,
    locked_for_animation: bool,
}

impl BoundedQueue {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(QueueState {
                slots: vec![None; capacity],
                first: 0,
                last: 0,
                count: 0,
                waiting_producers: 0,
                waiting_consumers: 0,
                locked_for_animation: false,
            }),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
        }
    }

    fn producer_begin(&self, running: &AtomicBool) -> Option<(usize, QueueSnapshot)> {
        let mut guard = self.state.lock();
        guard.waiting_producers += 1;
        while (guard.count == guard.slots.len() || guard.locked_for_animation)
            && running.load(Ordering::Relaxed)
        {
            self.not_full.wait_for(&mut guard, Duration::from_millis(100));
        }
        guard.waiting_producers = guard.waiting_producers.saturating_sub(1);
        if !running.load(Ordering::Relaxed) {
            return None;
        }
        guard.locked_for_animation = true;
        let slot_idx = guard.last;
        let snapshot = QueueSnapshot {
            slots: guard.slots.clone(),
            count: guard.count,
            waiting_producers: guard.waiting_producers,
            waiting_consumers: guard.waiting_consumers,
        };
        Some((slot_idx, snapshot))
    }

    fn producer_commit(&self, slot_idx: usize, item: QueueItem) -> QueueSnapshot {
        let mut guard = self.state.lock();
        guard.slots[slot_idx] = Some(item);
        guard.last = (guard.last + 1) % guard.slots.len();
        guard.count += 1;
        guard.locked_for_animation = false;
        let snapshot = QueueSnapshot {
            slots: guard.slots.clone(),
            count: guard.count,
            waiting_producers: guard.waiting_producers,
            waiting_consumers: guard.waiting_consumers,
        };
        self.not_empty.notify_one();
        self.not_full.notify_all();
        snapshot
    }

    fn consumer_begin(&self, running: &AtomicBool) -> Option<(usize, QueueItem, QueueSnapshot)> {
        let mut guard = self.state.lock();
        guard.waiting_consumers += 1;
        while (guard.count == 0 || guard.locked_for_animation) && running.load(Ordering::Relaxed) {
            self.not_empty.wait_for(&mut guard, Duration::from_millis(100));
        }
        guard.waiting_consumers = guard.waiting_consumers.saturating_sub(1);
        if !running.load(Ordering::Relaxed) {
            return None;
        }
        guard.locked_for_animation = true;
        let slot_idx = guard.first;
        let item = guard.slots[slot_idx].take()?;
        guard.first = (guard.first + 1) % guard.slots.len();
        guard.count = guard.count.saturating_sub(1);
        let snapshot = QueueSnapshot {
            slots: guard.slots.clone(),
            count: guard.count,
            waiting_producers: guard.waiting_producers,
            waiting_consumers: guard.waiting_consumers,
        };
        Some((slot_idx, item, snapshot))
    }

    fn consumer_commit(&self) -> QueueSnapshot {
        let mut guard = self.state.lock();
        guard.locked_for_animation = false;
        let snapshot = QueueSnapshot {
            slots: guard.slots.clone(),
            count: guard.count,
            waiting_producers: guard.waiting_producers,
            waiting_consumers: guard.waiting_consumers,
        };
        self.not_full.notify_one();
        self.not_empty.notify_all();
        snapshot
    }

    fn snapshot(&self) -> QueueSnapshot {
        let guard = self.state.lock();
        QueueSnapshot {
            slots: guard.slots.clone(),
            count: guard.count,
            waiting_producers: guard.waiting_producers,
            waiting_consumers: guard.waiting_consumers,
        }
    }

    fn wake_all(&self) {
        self.not_empty.notify_all();
        self.not_full.notify_all();
    }
}

fn producer_worker(
    id: usize,
    shared: Arc<RwLock<AppState>>,
    queue: Arc<BoundedQueue>,
    running: Arc<AtomicBool>,
    control: Arc<RunControl>,
) {
    let mut rng = SmallRng::seeded(0xFACE0000 + id as u64 * 157);
    let mut item_seq = 0_u64;

    while running.load(Ordering::Relaxed) {
        if !control.wait_if_paused(&running) {
            break;
        }

        let color = Colors::producer_palette(id).gamma_multiply(0.75 + 0.25 * rng.next_f32());
        let item_id = ((id as u64) << 32) | item_seq;
        item_seq += 1;
        {
            let mut app = shared.write();
            app.set_actor(ActorKind::Producer, id, ActorState::ActiveColor, color);
        }

        if !sleep_with_pause(
            &running,
            &control,
            random_wait_duration(&mut rng),
        ) {
            break;
        }

        {
            let mut app = shared.write();
            app.set_actor(ActorKind::Producer, id, ActorState::Waiting, Color32::BLACK);
            app.add_moving_item(item_id, color, producer_item_origin(id));
        }

        let queue_item = QueueItem {
            id: item_id,
            color,
            producer_id: id,
        };

        let Some((slot_idx, snapshot)) = queue.producer_begin(&running) else {
            shared.write().remove_moving_item(item_id);
            break;
        };

        {
            let mut app = shared.write();
            app.set_actor(ActorKind::Producer, id, ActorState::HoldingLock, Color32::WHITE);
            app.set_queue_snapshot(&snapshot);
        }

        let from = producer_item_origin(id);
        let to = queue_slot_center(slot_idx);
        if !animate_item(&shared, &running, &control, item_id, from, to, ITEM_ANIMATION_MS) {
            shared.write().remove_moving_item(item_id);
            queue.wake_all();
            break;
        }

        let snapshot = queue.producer_commit(slot_idx, queue_item);
        {
            let mut app = shared.write();
            app.remove_moving_item(item_id);
            app.set_queue_snapshot(&snapshot);
            app.increment_actor_count(ActorKind::Producer, id, Color32::WHITE);
        }
    }

    shared.write().set_actor(ActorKind::Producer, id, ActorState::Stopped, Color32::from_gray(90));
}

fn consumer_worker(
    id: usize,
    shared: Arc<RwLock<AppState>>,
    queue: Arc<BoundedQueue>,
    running: Arc<AtomicBool>,
    control: Arc<RunControl>,
) {
    let mut rng = SmallRng::seeded(0xBEEF1000 + id as u64 * 193);

    while running.load(Ordering::Relaxed) {
        if !control.wait_if_paused(&running) {
            break;
        }

        if !sleep_with_pause(
            &running,
            &control,
            random_wait_duration(&mut rng),
        ) {
            break;
        }

        {
            let mut app = shared.write();
            app.set_actor(ActorKind::Consumer, id, ActorState::Waiting, Color32::BLACK);
        }

        let Some((slot_idx, item, snapshot)) = queue.consumer_begin(&running) else {
            break;
        };

        {
            let mut app = shared.write();
            app.set_actor(ActorKind::Consumer, id, ActorState::HoldingLock, Color32::WHITE);
            app.set_queue_snapshot(&snapshot);
            app.add_moving_item(item.id, item.color, queue_slot_center(slot_idx));
        }

        let from = queue_slot_center(slot_idx);
        let to = consumer_item_target(id);
        if !animate_item(&shared, &running, &control, item.id, from, to, ITEM_ANIMATION_MS) {
            shared.write().remove_moving_item(item.id);
            queue.wake_all();
            break;
        }

        let snapshot = queue.consumer_commit();
        {
            let mut app = shared.write();
            app.remove_moving_item(item.id);
            app.set_queue_snapshot(&snapshot);
            app.increment_actor_count(ActorKind::Consumer, id, item.color);
        }
    }

    shared.write().set_actor(ActorKind::Consumer, id, ActorState::Stopped, Color32::from_gray(90));
}

fn animate_item(
    shared: &Arc<RwLock<AppState>>,
    running: &AtomicBool,
    control: &RunControl,
    item_id: u64,
    from: Pos2,
    to: Pos2,
    duration_ms: u64,
) -> bool {
    let steps = 20;
    let step_duration = Duration::from_millis((duration_ms / steps as u64).max(1));

    for step in 0..=steps {
        if !control.wait_if_paused(running) {
            return false;
        }
        let t = step as f32 / steps as f32;
        let pos = Pos2::new(
            from.x + (to.x - from.x) * t,
            from.y + (to.y - from.y) * t,
        );
        shared.write().move_item(item_id, pos);
        if step < steps && !sleep_with_pause(running, control, step_duration) {
            return false;
        }
    }

    true
}

fn sleep_with_pause(running: &AtomicBool, control: &RunControl, duration: Duration) -> bool {
    let start = Instant::now();
    let chunk = Duration::from_millis(25);
    while running.load(Ordering::Relaxed) && start.elapsed() < duration {
        if !control.wait_if_paused(running) {
            return false;
        }
        let remaining = duration.saturating_sub(start.elapsed());
        thread::sleep(remaining.min(chunk));
    }
    running.load(Ordering::Relaxed)
}

fn random_wait_duration(rng: &mut SmallRng) -> Duration {
    Duration::from_millis(THINK_MIN_MS + rng.next_u64() % THINK_RANGE_MS.max(1))
}

fn producer_center(id: usize) -> Pos2 {
    Pos2::new(PRODUCER_X, ACTOR_START_Y + id as f32 * ACTOR_Y_STEP)
}

fn producer_item_origin(id: usize) -> Pos2 {
    producer_center(id) + Vec2::new(52.0, 0.0)
}

fn consumer_center(id: usize) -> Pos2 {
    Pos2::new(CONSUMER_X, ACTOR_START_Y + id as f32 * ACTOR_Y_STEP)
}

fn consumer_item_target(id: usize) -> Pos2 {
    consumer_center(id) + Vec2::new(-54.0, 0.0)
}

fn queue_slot_center(slot_idx: usize) -> Pos2 {
    // Match the original TSGL ProducerConsumer demo:
    // item angle = (i * 2π + π) / CAPACITY, which places the item between
    // the radial divider lines rather than directly on top of one.
    let angle = (slot_idx as f32 * TAU + PI) / CAPACITY as f32;
    Pos2::new(
        QUEUE_CENTER_X + SLOT_RADIUS * angle.cos(),
        QUEUE_CENTER_Y + SLOT_RADIUS * angle.sin(),
    )
}

fn draw_queue_frame(painter: &egui::Painter) {
    painter.circle_stroke(
        Pos2::new(QUEUE_CENTER_X, QUEUE_CENTER_Y),
        OUTER_RADIUS,
        Stroke::new(2.0, Color32::BLACK),
    );
    painter.circle_stroke(
        Pos2::new(QUEUE_CENTER_X, QUEUE_CENTER_Y),
        INNER_RADIUS,
        Stroke::new(2.0, Color32::BLACK),
    );

    for idx in 0..CAPACITY {
        let angle = (idx as f32 * TAU) / CAPACITY as f32;
        let inner = Pos2::new(
            QUEUE_CENTER_X - INNER_RADIUS * angle.sin(),
            QUEUE_CENTER_Y + INNER_RADIUS * angle.cos(),
        );
        let outer = Pos2::new(
            QUEUE_CENTER_X - OUTER_RADIUS * angle.sin(),
            QUEUE_CENTER_Y + OUTER_RADIUS * angle.cos(),
        );
        painter.line_segment([inner, outer], Stroke::new(1.5, Color32::BLACK));
    }
}

fn draw_actor(painter: &egui::Painter, actor: &ActorVisual, center: Pos2, circle: bool) {
    let fill = match actor.state {
        ActorState::ActiveColor => actor.color,
        ActorState::Waiting => Color32::BLACK,
        ActorState::HoldingLock => Color32::WHITE,
        ActorState::Stopped => actor.color,
    };

    if circle {
        painter.circle_filled(center, 19.0, fill);
        painter.circle_stroke(center, 19.0, Stroke::new(2.0, Color32::BLACK));
    } else {
        let rect = Rect::from_center_size(center, Vec2::new(38.0, 38.0));
        painter.rect_filled(rect, 2.0, fill);
        painter.rect_stroke(rect, 2.0, Stroke::new(2.0, Color32::BLACK));
    }

    let text_color = if fill == Color32::BLACK {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    painter.text(
        center,
        Align2::CENTER_CENTER,
        actor.count.to_string(),
        FontId::proportional(16.0),
        text_color,
    );

    let label_offset = if circle { -44.0 } else { 52.0 };
    let label_anchor = if circle {
        Align2::RIGHT_CENTER
    } else {
        Align2::LEFT_CENTER
    };
    painter.text(
        center + Vec2::new(label_offset, 0.0),
        label_anchor,
        format!("{} {}", actor.kind.label(), actor.id + 1),
        FontId::proportional(15.0),
        Color32::BLACK,
    );
    painter.text(
        center + Vec2::new(0.0, 28.0),
        Align2::CENTER_CENTER,
        actor.state.label(actor.kind),
        FontId::proportional(13.0),
        Color32::from_rgb(70, 70, 70),
    );
}

fn draw_legend(painter: &egui::Painter) {
    let top = 545.0;
    painter.text(
        Pos2::new(WINDOW_SIZE[0] * 0.5, top - 30.0),
        Align2::CENTER_CENTER,
        "Legend",
        FontId::proportional(22.0),
        Color32::BLACK,
    );

    draw_legend_actor(
        painter,
        Pos2::new(180.0, top),
        true,
        Colors::producer_palette(0),
        "producing",
    );
    draw_legend_actor(
        painter,
        Pos2::new(180.0, top + 60.0),
        true,
        Color32::BLACK,
        "waiting for lock",
    );
    draw_legend_actor(
        painter,
        Pos2::new(180.0, top + 120.0),
        true,
        Color32::WHITE,
        "holding lock",
    );

    draw_legend_actor(
        painter,
        Pos2::new(560.0, top),
        false,
        Colors::consumer_palette(0),
        "consuming",
    );
    draw_legend_actor(
        painter,
        Pos2::new(560.0, top + 60.0),
        false,
        Color32::BLACK,
        "waiting for lock",
    );
    draw_legend_actor(
        painter,
        Pos2::new(560.0, top + 120.0),
        false,
        Color32::WHITE,
        "holding lock",
    );
}

fn draw_legend_actor(
    painter: &egui::Painter,
    center: Pos2,
    circle: bool,
    fill: Color32,
    label: &str,
) {
    if circle {
        painter.circle_filled(center, 18.0, fill);
        painter.circle_stroke(center, 18.0, Stroke::new(1.5, Color32::BLACK));
    } else {
        let rect = Rect::from_center_size(center, Vec2::new(36.0, 36.0));
        painter.rect_filled(rect, 2.0, fill);
        painter.rect_stroke(rect, 2.0, Stroke::new(1.5, Color32::BLACK));
    }
    painter.text(
        center + Vec2::new(54.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(18.0),
        Color32::BLACK,
    );
}

fn draw_star(
    painter: &egui::Painter,
    center: Pos2,
    outer_radius: f32,
    inner_radius: f32,
    points: usize,
    fill: Color32,
    stroke: Stroke,
) {
    let mut vertices = Vec::with_capacity(points * 2);
    for index in 0..(points * 2) {
        let radius = if index % 2 == 0 {
            outer_radius
        } else {
            inner_radius
        };
        let angle = -PI / 2.0 + index as f32 * PI / points as f32;
        vertices.push(Pos2::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        ));
    }
    painter.add(Shape::convex_polygon(vertices, fill, stroke));
}

struct Colors;

impl Colors {
    fn producer_palette(id: usize) -> Color32 {
        const PALETTE: [Color32; 8] = [
            Color32::from_rgb(205, 62, 59),
            Color32::from_rgb(214, 117, 51),
            Color32::from_rgb(206, 166, 54),
            Color32::from_rgb(96, 166, 77),
            Color32::from_rgb(53, 144, 202),
            Color32::from_rgb(104, 96, 197),
            Color32::from_rgb(188, 83, 145),
            Color32::from_rgb(47, 170, 161),
        ];
        PALETTE[id % PALETTE.len()]
    }

    fn consumer_palette(id: usize) -> Color32 {
        Self::producer_palette(id)
    }
}

struct SmallRng {
    state: u64,
}

impl SmallRng {
    fn seeded(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    fn next_f32(&mut self) -> f32 {
        (self.next_u64() as f64 / u64::MAX as f64) as f32
    }
}
