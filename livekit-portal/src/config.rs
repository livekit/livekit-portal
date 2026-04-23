use crate::dtype::DType;
use crate::types::{Role, SyncConfig};

/// A single schema entry: field name plus declared on-wire dtype.
///
/// Named for parity with the UniFFI-facing `FieldSpec` record the
/// bindings expose. Tuple form `(name, dtype)` is still accepted by the
/// `add_*_typed` methods — `FieldSpec` is the self-documenting
/// alternative.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSpec {
    pub name: String,
    pub dtype: DType,
}

impl FieldSpec {
    pub fn new(name: impl Into<String>, dtype: DType) -> Self {
        Self { name: name.into(), dtype }
    }
}

impl<S: Into<String>> From<(S, DType)> for FieldSpec {
    fn from((name, dtype): (S, DType)) -> Self {
        Self { name: name.into(), dtype }
    }
}

impl From<FieldSpec> for (String, DType) {
    fn from(f: FieldSpec) -> Self {
        (f.name, f.dtype)
    }
}

/// Configuration for a Portal session. Built incrementally before connecting.
#[derive(Debug, Clone)]
pub struct PortalConfig {
    pub(crate) session: String,
    pub(crate) role: Role,
    pub(crate) video_tracks: Vec<String>,
    pub(crate) state_schema: Vec<(String, DType)>,
    pub(crate) action_schema: Vec<(String, DType)>,
    pub(crate) state_reliable: bool,
    pub(crate) action_reliable: bool,
    pub(crate) fps: u32,
    pub(crate) slack: u32,
    pub(crate) tolerance: f32,
    pub(crate) ping_ms: u64,
}

impl PortalConfig {
    pub fn new(session: impl Into<String>, role: Role) -> Self {
        Self {
            session: session.into(),
            role,
            video_tracks: Vec::new(),
            state_schema: Vec::new(),
            action_schema: Vec::new(),
            state_reliable: true,
            action_reliable: true,
            fps: 30,
            slack: 5,
            tolerance: 1.5,
            ping_ms: 1000,
        }
    }

    pub fn add_video(&mut self, name: impl Into<String>) {
        self.video_tracks.push(name.into());
    }

    /// Declare state fields with per-field dtype. Order is significant and
    /// must match on both peers. Appends to any previous declaration.
    ///
    /// Accepts anything iterable yielding a `FieldSpec` or anything
    /// convertible to one — `&[(&str, DType)]`, `[FieldSpec, ...]`,
    /// `Vec<(String, DType)>`, mapped iterators.
    pub fn add_state_typed<F, I>(&mut self, schema: I)
    where
        F: Into<FieldSpec>,
        I: IntoIterator<Item = F>,
    {
        self.state_schema
            .extend(schema.into_iter().map(|f| f.into()).map(|f| (f.name, f.dtype)));
    }

    /// Declare action fields with per-field dtype. Order is significant and
    /// must match on both peers. Appends to any previous declaration.
    ///
    /// Same input flexibility as `add_state_typed`.
    pub fn add_action_typed<F, I>(&mut self, schema: I)
    where
        F: Into<FieldSpec>,
        I: IntoIterator<Item = F>,
    {
        self.action_schema
            .extend(schema.into_iter().map(|f| f.into()).map(|f| (f.name, f.dtype)));
    }

    /// Unified observation rate (set to the video capture rate if state and
    /// video differ). Drives `search_range = tolerance/fps`.
    pub fn set_fps(&mut self, fps: u32) {
        assert!(fps > 0, "fps must be > 0");
        self.fps = fps;
    }

    /// How far (in tick intervals at `fps`) a state may reach when matching
    /// a video frame. `search_range = tolerance / fps`.
    ///
    /// - `0.5` (tight): state only matches a frame within ±half a tick.
    ///   One lost frame → one dropped observation. Lowest misalignment risk.
    /// - `1.5` (default, widened): state matches its own frame, or falls
    ///   back to T±1 if its native frame was lost. Preserves observations
    ///   at the cost of occasional ±1-tick misalignment. A fair-share check
    ///   prevents an earlier state from stealing a frame closer to a later
    ///   state already in the buffer.
    /// - `> 2.0`: state may match T±2 frames. Higher recovery, higher
    ///   misalignment risk. Rarely worth it.
    ///
    /// Values must be in `(0, ∞)`. Defaults to `1.5`.
    pub fn set_tolerance(&mut self, ticks: f32) {
        assert!(ticks > 0.0, "tolerance must be > 0");
        self.tolerance = ticks;
    }

    /// Ticks of pipeline headroom — how much jitter, loss-detection latency,
    /// and consumer lag the pipeline tolerates before dropping. Applies to
    /// the per-track video sync buffer, the state sync buffer, and the
    /// pull-side observation buffer.
    pub fn set_slack(&mut self, ticks: u32) {
        assert!(ticks > 0, "slack must be > 0");
        self.slack = ticks;
    }

    pub fn set_state_reliable(&mut self, reliable: bool) {
        self.state_reliable = reliable;
    }

    pub fn set_action_reliable(&mut self, reliable: bool) {
        self.action_reliable = reliable;
    }

    /// RTT ping cadence. Set to `0` to disable active pinging on this side;
    /// the pong echo path remains active so the peer can still measure.
    pub fn set_ping_ms(&mut self, ms: u64) {
        self.ping_ms = ms;
    }

    pub fn video_tracks(&self) -> &[String] {
        &self.video_tracks
    }

    /// Ordered state field names. Derived from `state_schema`; does not
    /// allocate.
    pub fn state_fields(&self) -> impl Iterator<Item = &str> {
        self.state_schema.iter().map(|(n, _)| n.as_str())
    }

    /// Ordered action field names. Derived from `action_schema`; does not
    /// allocate.
    pub fn action_fields(&self) -> impl Iterator<Item = &str> {
        self.action_schema.iter().map(|(n, _)| n.as_str())
    }

    /// Full state schema (name + dtype).
    pub fn state_schema(&self) -> &[(String, DType)] {
        &self.state_schema
    }

    /// Full action schema (name + dtype).
    pub fn action_schema(&self) -> &[(String, DType)] {
        &self.action_schema
    }

    /// Derived sync config used internally by the sync buffer. Not public.
    pub(crate) fn sync_config(&self) -> SyncConfig {
        let search_range_us = (self.tolerance * 1_000_000.0 / self.fps as f32) as u64;
        SyncConfig {
            video_buffer_size: self.slack,
            state_buffer_size: self.slack,
            search_range_us,
        }
    }
}
