//! Typed modular graph (SYN-07): model, compiler and allocation-free runtime.
//!
//! Two subgraphs live in a patch: the **voice** graph runs once per active voice
//! between the layer generators and the voice filter/amp stage, the **global**
//! graph runs once on the summed voice output before the master gain. An empty
//! subgraph is a pass-through, so basic patches never need the graph.
//!
//! Port types are `Audio` (mono, audio rate), `Stereo`, `Control` (one value per
//! control interval) and `Event` (gate signal). The compiler rejects type
//! mismatches, dangling references, multiple edges into one input, limits and
//! instantaneous cycles. Feedback is permitted only through the input of a
//! `Delay` node, whose read position is at least [`FEEDBACK_QUANTUM`] samples
//! behind the write head; the compiler schedules the delayed *read* before and
//! the *write* after everything else, so a cycle through it is never evaluated
//! instantaneously. Feedback gain is bounded below unity and soft-clipped.

use crate::expr::{self, Evaluator, Program};
use crate::voice::CONTROL_INTERVAL;
use rawdawg_dsp::envelope::{Adsr, AdsrParams};
use rawdawg_dsp::filter::{FilterMode, Svf};
use rawdawg_dsp::math::fast_tanh;
use rawdawg_dsp::oscillator::{BasicOscillator, BasicTables, BasicWave};
use rawdawg_dsp::rng::DeterministicRng;
use rawdawg_types::params::ParamCategory;
use serde::{Deserialize, Serialize};

pub const GRAPH_VERSION: u32 = 1;
pub const MAX_NODES: usize = 64;
pub const MAX_EDGES: usize = 128;
/// Processing sub-block (one control interval).
pub const SUB_BLOCK: usize = CONTROL_INTERVAL;
/// Minimum delay (samples) of a `Delay` node; one prepared feedback quantum.
pub const FEEDBACK_QUANTUM: usize = 64;
/// Longest `Delay` node time per scope.
pub const MAX_DELAY_S_VOICE: f32 = 0.5;
pub const MAX_DELAY_S_GLOBAL: f32 = 2.0;
/// Feedback gain is clamped below unity.
pub const MAX_FEEDBACK: f32 = 0.95;
/// Approximate cost budgets (units per sub-block, see [`NodeKind::cost`]).
pub const MAX_VOICE_COST: u32 = 192;
pub const MAX_GLOBAL_COST: u32 = 384;
pub const MAX_PARAMS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphNodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortType {
    Audio,
    Stereo,
    Control,
    Event,
}

impl PortType {
    pub fn label(self) -> &'static str {
        match self {
            PortType::Audio => "audio",
            PortType::Stereo => "stereo",
            PortType::Control => "control",
            PortType::Event => "event",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    Voice,
    Global,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    VoiceIn,
    VoiceOut,
    GlobalIn,
    GlobalOut,
    Osc,
    Noise,
    Env,
    Lfo,
    Const,
    Math,
    Mix,
    StereoMix,
    Filter,
    Gain,
    Delay,
    Shaper,
    Z1,
    ToStereo,
    ToMono,
    ControlToAudio,
    AudioToControl,
    Reduce,
}

/// Static port description. `default` is the value an unconnected control input
/// reads; `param` names a node parameter that overrides that default.
#[derive(Clone, Copy, Debug)]
pub struct Port {
    pub name: &'static str,
    pub ty: PortType,
    pub default: f32,
    pub param: Option<usize>,
}

const fn port(name: &'static str, ty: PortType) -> Port {
    Port { name, ty, default: 0.0, param: None }
}
const fn ctrl(name: &'static str, default: f32) -> Port {
    Port { name, ty: PortType::Control, default, param: None }
}
const fn ctrl_param(name: &'static str, param: usize) -> Port {
    Port { name, ty: PortType::Control, default: 0.0, param: Some(param) }
}

/// Static node parameter description (physical units).
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// Perturb/interpolate in the log domain.
    pub log: bool,
    /// Integer-valued choice; `labels` names the steps.
    pub labels: &'static [&'static str],
    pub category: ParamCategory,
}

const fn lin(name: &'static str, min: f32, max: f32, default: f32, category: ParamCategory) -> ParamSpec {
    ParamSpec { name, min, max, default, log: false, labels: &[], category }
}
const fn log(name: &'static str, min: f32, max: f32, default: f32, category: ParamCategory) -> ParamSpec {
    ParamSpec { name, min, max, default, log: true, labels: &[], category }
}
const fn choice(name: &'static str, labels: &'static [&'static str], default: f32, category: ParamCategory) -> ParamSpec {
    ParamSpec { name, min: 0.0, max: (labels.len() - 1) as f32, default, log: false, labels, category }
}

pub const OSC_WAVES: [&str; 4] = ["Sine", "Triangle", "Saw", "Pulse"];
pub const LFO_SHAPES: [&str; 5] = ["Sine", "Triangle", "Saw", "Square", "S&H"];
pub const FILTER_MODES: [&str; 4] = ["Low-pass", "High-pass", "Band-pass", "Notch"];
pub const SHAPER_MODES: [&str; 4] = ["Tanh", "Fold", "Clip", "Sine"];
pub const REDUCE_MODES: [&str; 4] = ["Sum", "Max", "Average", "Last"];
pub const A2C_MODES: [&str; 2] = ["Level", "Sample"];

impl NodeKind {
    pub const ALL: [NodeKind; 22] = [
        NodeKind::VoiceIn,
        NodeKind::VoiceOut,
        NodeKind::GlobalIn,
        NodeKind::GlobalOut,
        NodeKind::Osc,
        NodeKind::Noise,
        NodeKind::Env,
        NodeKind::Lfo,
        NodeKind::Const,
        NodeKind::Math,
        NodeKind::Mix,
        NodeKind::StereoMix,
        NodeKind::Filter,
        NodeKind::Gain,
        NodeKind::Delay,
        NodeKind::Shaper,
        NodeKind::Z1,
        NodeKind::ToStereo,
        NodeKind::ToMono,
        NodeKind::ControlToAudio,
        NodeKind::AudioToControl,
        NodeKind::Reduce,
    ];

    /// Kinds a user may add (in/out nodes are fixed).
    pub const USER: [NodeKind; 18] = [
        NodeKind::Osc,
        NodeKind::Noise,
        NodeKind::Env,
        NodeKind::Lfo,
        NodeKind::Const,
        NodeKind::Math,
        NodeKind::Mix,
        NodeKind::StereoMix,
        NodeKind::Filter,
        NodeKind::Gain,
        NodeKind::Delay,
        NodeKind::Shaper,
        NodeKind::Z1,
        NodeKind::ToStereo,
        NodeKind::ToMono,
        NodeKind::ControlToAudio,
        NodeKind::AudioToControl,
        NodeKind::Reduce,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NodeKind::VoiceIn => "Voice in",
            NodeKind::VoiceOut => "Voice out",
            NodeKind::GlobalIn => "Mix in",
            NodeKind::GlobalOut => "Mix out",
            NodeKind::Osc => "Oscillator",
            NodeKind::Noise => "Noise",
            NodeKind::Env => "Envelope",
            NodeKind::Lfo => "LFO",
            NodeKind::Const => "Constant",
            NodeKind::Math => "Expression",
            NodeKind::Mix => "Mix",
            NodeKind::StereoMix => "Stereo mix",
            NodeKind::Filter => "Filter",
            NodeKind::Gain => "Gain",
            NodeKind::Delay => "Delay (feedback)",
            NodeKind::Shaper => "Waveshaper",
            NodeKind::Z1 => "z⁻¹",
            NodeKind::ToStereo => "Mono → stereo",
            NodeKind::ToMono => "Stereo → mono",
            NodeKind::ControlToAudio => "Control → audio",
            NodeKind::AudioToControl => "Audio → control",
            NodeKind::Reduce => "Reduce",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            NodeKind::VoiceIn => "The voice's layers, pitch, gate, velocity, envelopes and LFOs",
            NodeKind::VoiceOut => "What this voice sends on to the filter and amp envelope",
            NodeKind::GlobalIn => "All voices summed, plus the global LFOs",
            NodeKind::GlobalOut => "What the synth outputs (before master gain)",
            NodeKind::Osc => "Bandlimited oscillator with phase-modulation input; follows the voice pitch when unconnected",
            NodeKind::Noise => "White noise with a colour control",
            NodeKind::Env => "ADSR envelope; follows the voice gate when unconnected",
            NodeKind::Lfo => "Low-frequency oscillator, bipolar output",
            NodeKind::Const => "A fixed control value",
            NodeKind::Math => "Your own bounded expression evaluated per sample",
            NodeKind::Mix => "Sum up to four mono signals with gains",
            NodeKind::StereoMix => "Sum up to four stereo signals with gains",
            NodeKind::Filter => "State-variable filter with cutoff/resonance modulation",
            NodeKind::Gain => "Gain in dB with a control multiplier",
            NodeKind::Delay => "Delay line; the only node that may close a feedback loop (read ≥ 64 samples behind)",
            NodeKind::Shaper => "Waveshaper with drive",
            NodeKind::Z1 => "One-sample delay (explicit state, no feedback)",
            NodeKind::ToStereo => "Pan a mono signal into a stereo pair",
            NodeKind::ToMono => "Average a stereo pair",
            NodeKind::ControlToAudio => "Ramp a control value to audio rate",
            NodeKind::AudioToControl => "Reduce audio to a control value per interval",
            NodeKind::Reduce => "Combine several control signals explicitly",
        }
    }

    pub fn inputs(self) -> &'static [Port] {
        use PortType::*;
        match self {
            NodeKind::VoiceIn | NodeKind::GlobalIn | NodeKind::Noise | NodeKind::Const => &[],
            NodeKind::VoiceOut | NodeKind::GlobalOut => {
                const P: &[Port] = &[port("in", Stereo)];
                P
            }
            NodeKind::Osc => {
                const P: &[Port] = &[ctrl("pitch", 69.0 / 127.0), port("fm", Audio), ctrl("pitch mod", 0.0)];
                P
            }
            NodeKind::Env => {
                const P: &[Port] = &[port("gate", Event)];
                P
            }
            NodeKind::Lfo => {
                const P: &[Port] = &[ctrl("rate mod", 0.0)];
                P
            }
            NodeKind::Math => {
                const P: &[Port] = &[
                    port("in1", Audio),
                    port("in2", Audio),
                    port("in3", Audio),
                    port("in4", Audio),
                    ctrl_param("ctrl1", 0),
                    ctrl_param("ctrl2", 1),
                    ctrl_param("ctrl3", 2),
                    ctrl_param("ctrl4", 3),
                ];
                P
            }
            NodeKind::Mix => {
                const P: &[Port] = &[port("a", Audio), port("b", Audio), port("c", Audio), port("d", Audio)];
                P
            }
            NodeKind::StereoMix => {
                const P: &[Port] = &[port("a", Stereo), port("b", Stereo), port("c", Stereo), port("d", Stereo)];
                P
            }
            NodeKind::Filter => {
                const P: &[Port] = &[port("in", Audio), ctrl("cutoff mod", 0.0), ctrl("res mod", 0.0)];
                P
            }
            NodeKind::Gain => {
                const P: &[Port] = &[port("in", Audio), ctrl("gain", 1.0)];
                P
            }
            NodeKind::Delay => {
                const P: &[Port] = &[port("in", Audio), ctrl("time mod", 0.0), ctrl("feedback mod", 0.0)];
                P
            }
            NodeKind::Shaper => {
                const P: &[Port] = &[port("in", Audio), ctrl("drive mod", 0.0)];
                P
            }
            NodeKind::Z1 => {
                const P: &[Port] = &[port("in", Audio)];
                P
            }
            NodeKind::ToStereo => {
                const P: &[Port] = &[port("in", Audio), ctrl("pan mod", 0.0)];
                P
            }
            NodeKind::ToMono => {
                const P: &[Port] = &[port("in", Stereo)];
                P
            }
            NodeKind::ControlToAudio => {
                const P: &[Port] = &[ctrl("in", 0.0)];
                P
            }
            NodeKind::AudioToControl => {
                const P: &[Port] = &[port("in", Audio)];
                P
            }
            NodeKind::Reduce => {
                const P: &[Port] = &[ctrl("a", 0.0), ctrl("b", 0.0), ctrl("c", 0.0), ctrl("d", 0.0)];
                P
            }
        }
    }

    pub fn outputs(self) -> &'static [Port] {
        use PortType::*;
        match self {
            NodeKind::VoiceIn => {
                const P: &[Port] = &[
                    port("mix", Stereo),
                    port("mono", Audio),
                    port("pitch", Control),
                    port("gate", Event),
                    port("velocity", Control),
                    port("amp env", Control),
                    port("filter env", Control),
                    port("lfo 1", Control),
                    port("lfo 2", Control),
                    port("lfo 3", Control),
                    port("lfo 4", Control),
                ];
                P
            }
            NodeKind::GlobalIn => {
                const P: &[Port] = &[port("mix", Stereo), port("mono", Audio), port("lfo 1", Control), port("lfo 2", Control), port("lfo 3", Control), port("lfo 4", Control)];
                P
            }
            NodeKind::VoiceOut | NodeKind::GlobalOut => &[],
            NodeKind::Osc
            | NodeKind::Noise
            | NodeKind::Math
            | NodeKind::Mix
            | NodeKind::Filter
            | NodeKind::Gain
            | NodeKind::Delay
            | NodeKind::Shaper
            | NodeKind::Z1
            | NodeKind::ToMono
            | NodeKind::ControlToAudio => {
                const P: &[Port] = &[port("out", Audio)];
                P
            }
            NodeKind::StereoMix | NodeKind::ToStereo => {
                const P: &[Port] = &[port("out", Stereo)];
                P
            }
            NodeKind::Env | NodeKind::Lfo | NodeKind::Const | NodeKind::AudioToControl | NodeKind::Reduce => {
                const P: &[Port] = &[port("out", Control)];
                P
            }
        }
    }

    pub fn params(self) -> &'static [ParamSpec] {
        use ParamCategory::*;
        match self {
            NodeKind::Osc => {
                const P: &[ParamSpec] = &[
                    choice("wave", &OSC_WAVES, 2.0, Wave),
                    lin("semitones", -48.0, 48.0, 0.0, Pitch),
                    lin("fine", -100.0, 100.0, 0.0, Pitch),
                    lin("fm index", 0.0, 10.0, 1.0, Wave),
                    lin("pulse width", 0.05, 0.95, 0.5, Wave),
                ];
                P
            }
            NodeKind::Noise => {
                const P: &[ParamSpec] = &[lin("colour", 0.0, 1.0, 1.0, Wave), lin("level", 0.0, 1.0, 0.5, Wave)];
                P
            }
            NodeKind::Env => {
                const P: &[ParamSpec] = &[
                    log("attack", 0.001, 10.0, 0.005, Envelope),
                    log("decay", 0.001, 10.0, 0.12, Envelope),
                    lin("sustain", 0.0, 1.0, 0.75, Envelope),
                    log("release", 0.001, 10.0, 0.18, Envelope),
                ];
                P
            }
            NodeKind::Lfo => {
                const P: &[ParamSpec] = &[
                    choice("shape", &LFO_SHAPES, 0.0, Modulation),
                    log("rate", 0.01, 50.0, 2.0, Modulation),
                    lin("depth", 0.0, 1.0, 1.0, Modulation),
                    lin("offset", -1.0, 1.0, 0.0, Modulation),
                ];
                P
            }
            NodeKind::Const => {
                const P: &[ParamSpec] = &[lin("value", -1.0, 1.0, 0.5, Modulation)];
                P
            }
            NodeKind::Math => {
                const P: &[ParamSpec] =
                    &[lin("ctrl1", 0.0, 1.0, 0.5, Effects), lin("ctrl2", 0.0, 1.0, 0.5, Effects), lin("ctrl3", 0.0, 1.0, 0.5, Effects), lin("ctrl4", 0.0, 1.0, 0.5, Effects)];
                P
            }
            NodeKind::Mix | NodeKind::StereoMix => {
                const P: &[ParamSpec] = &[lin("a", 0.0, 2.0, 1.0, Effects), lin("b", 0.0, 2.0, 1.0, Effects), lin("c", 0.0, 2.0, 1.0, Effects), lin("d", 0.0, 2.0, 1.0, Effects)];
                P
            }
            NodeKind::Filter => {
                const P: &[ParamSpec] = &[
                    choice("mode", &FILTER_MODES, 0.0, Filter),
                    log("cutoff", 20.0, 20_000.0, 2000.0, Filter),
                    log("resonance", 0.5, 20.0, 0.707, Filter),
                    lin("mod octaves", 0.0, 8.0, 4.0, Filter),
                ];
                P
            }
            NodeKind::Gain => {
                const P: &[ParamSpec] = &[lin("gain dB", -60.0, 24.0, 0.0, Effects)];
                P
            }
            NodeKind::Delay => {
                const P: &[ParamSpec] =
                    &[log("time ms", 1.5, 2000.0, 250.0, Effects), lin("feedback", 0.0, MAX_FEEDBACK, 0.4, Effects), lin("time mod ms", 0.0, 100.0, 0.0, Effects)];
                P
            }
            NodeKind::Shaper => {
                const P: &[ParamSpec] = &[choice("mode", &SHAPER_MODES, 0.0, Effects), log("drive", 1.0, 40.0, 3.0, Effects), lin("level", 0.0, 1.0, 0.7, Effects)];
                P
            }
            NodeKind::ToStereo => {
                const P: &[ParamSpec] = &[lin("pan", -1.0, 1.0, 0.0, Effects)];
                P
            }
            NodeKind::ControlToAudio => {
                const P: &[ParamSpec] = &[lin("scale", -4.0, 4.0, 1.0, Modulation)];
                P
            }
            NodeKind::AudioToControl => {
                const P: &[ParamSpec] = &[choice("mode", &A2C_MODES, 0.0, Modulation), lin("scale", 0.0, 8.0, 1.0, Modulation)];
                P
            }
            NodeKind::Reduce => {
                const P: &[ParamSpec] = &[choice("mode", &REDUCE_MODES, 0.0, Modulation)];
                P
            }
            _ => &[],
        }
    }

    /// Approximate processing cost in abstract units per sub-block.
    pub fn cost(self) -> u32 {
        match self {
            NodeKind::VoiceIn | NodeKind::VoiceOut | NodeKind::GlobalIn | NodeKind::GlobalOut | NodeKind::Const => 1,
            NodeKind::Osc | NodeKind::Filter => 6,
            NodeKind::Delay => 4,
            NodeKind::Math | NodeKind::Shaper | NodeKind::StereoMix => 3,
            NodeKind::Noise | NodeKind::Env | NodeKind::Lfo | NodeKind::Mix => 2,
            _ => 1,
        }
    }

    pub fn is_fixed(self) -> bool {
        matches!(self, NodeKind::VoiceIn | NodeKind::VoiceOut | NodeKind::GlobalIn | NodeKind::GlobalOut)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: GraphNodeId,
    pub kind: NodeKind,
    /// Physical parameter values, one per [`NodeKind::params`] entry.
    #[serde(default)]
    pub params: Vec<f32>,
    /// Expression source for [`NodeKind::Math`].
    #[serde(default)]
    pub expr: String,
    /// Editor position.
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
}

impl Node {
    pub fn new(id: GraphNodeId, kind: NodeKind, x: f32, y: f32) -> Self {
        Self {
            id,
            kind,
            params: kind.params().iter().map(|p| p.default).collect(),
            expr: if kind == NodeKind::Math { expr::EXAMPLES[0].1.to_owned() } else { String::new() },
            x,
            y,
        }
    }

    pub fn param(&self, i: usize) -> f32 {
        let spec = self.kind.params();
        match (self.params.get(i), spec.get(i)) {
            (Some(v), Some(s)) if v.is_finite() => v.clamp(s.min, s.max),
            (_, Some(s)) => s.default,
            _ => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub from: GraphNodeId,
    pub from_port: u8,
    pub to: GraphNodeId,
    pub to_port: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SubGraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl SubGraph {
    /// A pass-through graph with the fixed in/out nodes for `scope`.
    pub fn passthrough(scope: Scope) -> Self {
        let (i, o) = match scope {
            Scope::Voice => (NodeKind::VoiceIn, NodeKind::VoiceOut),
            Scope::Global => (NodeKind::GlobalIn, NodeKind::GlobalOut),
        };
        Self {
            nodes: vec![Node::new(GraphNodeId(1), i, 40.0, 80.0), Node::new(GraphNodeId(2), o, 520.0, 80.0)],
            edges: vec![Edge { from: GraphNodeId(1), from_port: 0, to: GraphNodeId(2), to_port: 0 }],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn next_id(&self) -> GraphNodeId {
        GraphNodeId(self.nodes.iter().map(|n| n.id.0).max().unwrap_or(0) + 1)
    }

    pub fn node(&self, id: GraphNodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: GraphNodeId) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn add_node(&mut self, kind: NodeKind, x: f32, y: f32) -> GraphNodeId {
        let id = self.next_id();
        self.nodes.push(Node::new(id, kind, x, y));
        id
    }

    /// Remove a node and every edge touching it. Fixed in/out nodes stay.
    pub fn remove_node(&mut self, id: GraphNodeId) {
        if self.node(id).is_some_and(|n| n.kind.is_fixed()) {
            return;
        }
        self.nodes.retain(|n| n.id != id);
        self.edges.retain(|e| e.from != id && e.to != id);
    }

    /// Connect, replacing any edge already feeding the target input.
    pub fn connect(&mut self, edge: Edge) {
        self.edges.retain(|e| !(e.to == edge.to && e.to_port == edge.to_port));
        self.edges.push(edge);
    }

    /// Topology (kinds, ids, edges) equal, ignoring parameter values and positions.
    pub fn same_topology(&self, other: &SubGraph) -> bool {
        self.nodes.len() == other.nodes.len()
            && self.nodes.iter().zip(&other.nodes).all(|(a, b)| a.id == b.id && a.kind == b.kind && (a.kind != NodeKind::Math || a.expr == b.expr))
            && self.edges == other.edges
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub version: u32,
    #[serde(default)]
    pub voice: SubGraph,
    #[serde(default)]
    pub global: SubGraph,
}

impl Default for Graph {
    fn default() -> Self {
        Self { version: GRAPH_VERSION, voice: SubGraph::default(), global: SubGraph::default() }
    }
}

impl Graph {
    pub fn is_empty(&self) -> bool {
        self.voice.is_empty() && self.global.is_empty()
    }

    pub fn sub(&self, scope: Scope) -> &SubGraph {
        match scope {
            Scope::Voice => &self.voice,
            Scope::Global => &self.global,
        }
    }

    pub fn sub_mut(&mut self, scope: Scope) -> &mut SubGraph {
        match scope {
            Scope::Voice => &mut self.voice,
            Scope::Global => &mut self.global,
        }
    }

    /// Validate both subgraphs (non-RT).
    pub fn validate(&self) -> Result<(), GraphError> {
        if self.version == 0 || self.version > GRAPH_VERSION {
            return Err(GraphError::Version(self.version));
        }
        compile(&self.voice, Scope::Voice)?;
        compile(&self.global, Scope::Global)?;
        Ok(())
    }

    pub fn same_topology(&self, other: &Graph) -> bool {
        self.voice.same_topology(&other.voice) && self.global.same_topology(&other.global)
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum GraphError {
    #[error("unsupported graph version {0}")]
    Version(u32),
    #[error("too many nodes ({0}, max {MAX_NODES})")]
    TooManyNodes(usize),
    #[error("too many edges ({0}, max {MAX_EDGES})")]
    TooManyEdges(usize),
    #[error("duplicate node id {0:?}")]
    DuplicateNode(GraphNodeId),
    #[error("node {0:?} ({1}) is not allowed in this graph")]
    WrongScope(GraphNodeId, &'static str),
    #[error("graph needs exactly one input and one output node")]
    MissingInOut,
    #[error("edge {edge} refers to a missing node")]
    MissingNode { edge: usize },
    #[error("edge {edge} refers to a missing port")]
    MissingPort { edge: usize },
    #[error("edge {edge}: {from} output cannot feed {to} input")]
    TypeMismatch { edge: usize, from: &'static str, to: &'static str },
    #[error("edge {edge}: input already connected (use Mix or Reduce to combine)")]
    DuplicateInput { edge: usize },
    #[error("instantaneous cycle through {path}; insert a Delay node to feed back")]
    Cycle { path: String, nodes: Vec<GraphNodeId> },
    #[error("graph cost {0} exceeds the budget of {1}")]
    TooExpensive(u32, u32),
    #[error("expression in node {node:?}: {error}")]
    Expression { node: GraphNodeId, error: expr::ExprError },
}

impl GraphError {
    /// Edge index this error points at, for highlighting.
    pub fn edge(&self) -> Option<usize> {
        match self {
            GraphError::MissingNode { edge } | GraphError::MissingPort { edge } | GraphError::TypeMismatch { edge, .. } | GraphError::DuplicateInput { edge } => Some(*edge),
            _ => None,
        }
    }
    pub fn nodes(&self) -> &[GraphNodeId] {
        match self {
            GraphError::Cycle { nodes, .. } => nodes,
            _ => &[],
        }
    }
}

/// Where a step reads an input from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Src {
    Audio(u16),
    /// Mono buffer widened to both channels.
    AudioAsStereo(u16),
    Stereo(u16),
    Ctrl(u16),
    Event(u16),
    Const(f32),
    /// Read node parameter `i` (unconnected control port with a parameter).
    Param(u8),
    Silence,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dst {
    Audio(u16),
    Stereo(u16),
    Ctrl(u16),
    Event(u16),
    /// Nobody reads this output.
    Unused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Node,
    DelayWrite,
}

#[derive(Clone, Debug)]
pub struct Step {
    pub node: usize,
    pub kind: StepKind,
    pub inputs: Vec<Src>,
    pub outputs: Vec<Dst>,
}

/// Prepared, immutable schedule. Built off-thread; executed on the RT thread.
#[derive(Clone, Debug)]
pub struct CompiledGraph {
    pub scope: Scope,
    pub nodes: Vec<Node>,
    pub programs: Vec<Option<Program>>,
    pub steps: Vec<Step>,
    pub audio_bufs: usize,
    pub stereo_bufs: usize,
    pub ctrl_slots: usize,
    pub event_bufs: usize,
    /// Buffers the runtime fills from the voice/global sources (indexed like the in node's outputs).
    pub source_dst: Vec<Dst>,
    /// Where the output node reads from.
    pub sink: Src,
    /// Edge indices that close a feedback loop through a Delay node.
    pub feedback_edges: Vec<usize>,
    pub cost: u32,
    /// Deterministic processing order of node ids.
    pub order: Vec<GraphNodeId>,
}

impl CompiledGraph {
    /// Node states of `other` can be carried into this graph (same kinds in the same slots).
    pub fn state_compatible(&self, other: &CompiledGraph) -> bool {
        self.scope == other.scope && self.nodes.len() == other.nodes.len() && self.nodes.iter().zip(&other.nodes).all(|(a, b)| a.id == b.id && a.kind == b.kind)
    }
}

fn kind_name(ty: PortType) -> &'static str {
    ty.label()
}

/// Compile a subgraph. Non-RT; allocates freely.
pub fn compile(g: &SubGraph, scope: Scope) -> Result<CompiledGraph, GraphError> {
    let n = g.nodes.len();
    if n > MAX_NODES {
        return Err(GraphError::TooManyNodes(n));
    }
    if g.edges.len() > MAX_EDGES {
        return Err(GraphError::TooManyEdges(g.edges.len()));
    }
    let (in_kind, out_kind) = match scope {
        Scope::Voice => (NodeKind::VoiceIn, NodeKind::VoiceOut),
        Scope::Global => (NodeKind::GlobalIn, NodeKind::GlobalOut),
    };
    if n == 0 {
        // pass-through
        return Ok(CompiledGraph {
            scope,
            nodes: Vec::new(),
            programs: Vec::new(),
            steps: Vec::new(),
            audio_bufs: 0,
            stereo_bufs: 1,
            ctrl_slots: 0,
            event_bufs: 0,
            source_dst: vec![Dst::Stereo(0)],
            sink: Src::Stereo(0),
            feedback_edges: Vec::new(),
            cost: 0,
            order: Vec::new(),
        });
    }
    for (i, a) in g.nodes.iter().enumerate() {
        if g.nodes[..i].iter().any(|b| b.id == a.id) {
            return Err(GraphError::DuplicateNode(a.id));
        }
        if a.kind.is_fixed() && a.kind != in_kind && a.kind != out_kind {
            return Err(GraphError::WrongScope(a.id, a.kind.label()));
        }
    }
    let ins: Vec<usize> = g.nodes.iter().enumerate().filter(|(_, x)| x.kind == in_kind).map(|(i, _)| i).collect();
    let outs: Vec<usize> = g.nodes.iter().enumerate().filter(|(_, x)| x.kind == out_kind).map(|(i, _)| i).collect();
    if ins.len() != 1 || outs.len() != 1 {
        return Err(GraphError::MissingInOut);
    }
    let (in_node, out_node) = (ins[0], outs[0]);
    let index_of = |id: GraphNodeId| g.nodes.iter().position(|x| x.id == id);

    // Resolve edges: (from node, from port, to node, to port)
    let mut resolved: Vec<(usize, usize, usize, usize)> = Vec::with_capacity(g.edges.len());
    for (ei, e) in g.edges.iter().enumerate() {
        let (Some(a), Some(b)) = (index_of(e.from), index_of(e.to)) else { return Err(GraphError::MissingNode { edge: ei }) };
        let (Some(op), Some(ip)) = (g.nodes[a].kind.outputs().get(e.from_port as usize), g.nodes[b].kind.inputs().get(e.to_port as usize)) else {
            return Err(GraphError::MissingPort { edge: ei });
        };
        let ok = op.ty == ip.ty || (op.ty == PortType::Audio && ip.ty == PortType::Stereo);
        if !ok {
            return Err(GraphError::TypeMismatch { edge: ei, from: kind_name(op.ty), to: kind_name(ip.ty) });
        }
        if resolved.iter().any(|&(_, _, tb, tp)| tb == b && tp == e.to_port as usize) {
            return Err(GraphError::DuplicateInput { edge: ei });
        }
        resolved.push((a, e.from_port as usize, b, e.to_port as usize));
    }

    // Expressions
    let mut programs: Vec<Option<Program>> = Vec::with_capacity(n);
    let mut cost = 0u32;
    for node in &g.nodes {
        if node.kind == NodeKind::Math {
            let p = expr::compile(&node.expr).map_err(|error| GraphError::Expression { node: node.id, error })?;
            cost += node.kind.cost() + (p.ops.len() as u32).div_ceil(4);
            programs.push(Some(p));
        } else {
            cost += node.kind.cost();
            programs.push(None);
        }
    }
    let budget = match scope {
        Scope::Voice => MAX_VOICE_COST,
        Scope::Global => MAX_GLOBAL_COST,
    };
    if cost > budget {
        return Err(GraphError::TooExpensive(cost, budget));
    }

    // Scheduling vertices: 2*i = node (Delay: read), 2*i+1 = Delay write.
    let vcount = 2 * n;
    let vertex_of_input = |node: usize| if g.nodes[node].kind == NodeKind::Delay { 2 * node + 1 } else { 2 * node };
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); vcount]; // deps[v] = vertices v depends on
    let mut users: Vec<Vec<usize>> = vec![Vec::new(); vcount];
    for &(a, _, b, _) in &resolved {
        let (from, to) = (2 * a, vertex_of_input(b));
        deps[to].push(from);
        users[from].push(to);
    }
    // Kahn with deterministic tie-break (lowest vertex first); write vertices only for Delay nodes.
    let active: Vec<bool> = (0..vcount).map(|v| v % 2 == 0 || g.nodes[v / 2].kind == NodeKind::Delay).collect();
    let mut indeg: Vec<usize> = deps.iter().map(|d| d.len()).collect();
    let mut done = vec![false; vcount];
    let mut order_v: Vec<usize> = Vec::with_capacity(vcount);
    while let Some(v) = (0..vcount).find(|&v| active[v] && !done[v] && indeg[v] == 0) {
        done[v] = true;
        order_v.push(v);
        for &u in &users[v] {
            indeg[u] -= 1;
        }
    }
    if order_v.len() != active.iter().filter(|a| **a).count() {
        // find a cycle among the remaining vertices
        let remaining: Vec<usize> = (0..vcount).filter(|&v| active[v] && !done[v]).collect();
        let cycle = find_cycle(&remaining, &users);
        let nodes: Vec<GraphNodeId> = cycle.iter().map(|&v| g.nodes[v / 2].id).collect();
        let path = cycle.iter().map(|&v| g.nodes[v / 2].kind.label()).collect::<Vec<_>>().join(" → ");
        return Err(GraphError::Cycle { path, nodes });
    }
    // Feedback edges: edges into Delay inputs whose source is downstream of the delay's output.
    let mut feedback_edges = Vec::new();
    for (ei, &(a, _, b, _)) in resolved.iter().enumerate() {
        if g.nodes[b].kind == NodeKind::Delay && reaches(2 * b, 2 * a, &users) {
            feedback_edges.push(ei);
        }
    }

    // Buffer allocation with liveness: consumers per (node, port).
    let mut consumers: Vec<Vec<u32>> = g.nodes.iter().map(|x| vec![0; x.kind.outputs().len()]).collect();
    for &(a, ap, _, _) in &resolved {
        consumers[a][ap] += 1;
    }
    let mut pools = Pools::default();
    let mut out_dst: Vec<Vec<Dst>> = g.nodes.iter().map(|x| vec![Dst::Unused; x.kind.outputs().len()]).collect();
    let mut steps: Vec<Step> = Vec::with_capacity(order_v.len());
    let mut order = Vec::with_capacity(n);
    for &v in &order_v {
        let ni = v / 2;
        let node = &g.nodes[ni];
        let kind = node.kind;
        let is_write = v % 2 == 1;
        let mut inputs: Vec<Src> = Vec::new();
        let mut outputs: Vec<Dst> = Vec::new();
        if !is_write {
            order.push(node.id);
        }
        // inputs (Delay: all inputs belong to the write step; read has none)
        let takes_inputs = if kind == NodeKind::Delay { is_write } else { true };
        if takes_inputs {
            for (pi, p) in kind.inputs().iter().enumerate() {
                let edge = resolved.iter().find(|&&(_, _, b, bp)| b == ni && bp == pi);
                let src = match edge {
                    Some(&(a, ap, _, _)) => {
                        let d = out_dst[a][ap];
                        let s = match (d, p.ty) {
                            (Dst::Audio(i), PortType::Audio) => Src::Audio(i),
                            (Dst::Audio(i), PortType::Stereo) => Src::AudioAsStereo(i),
                            (Dst::Stereo(i), _) => Src::Stereo(i),
                            (Dst::Ctrl(i), _) => Src::Ctrl(i),
                            (Dst::Event(i), _) => Src::Event(i),
                            _ => Src::Silence,
                        };
                        consumers[a][ap] -= 1;
                        if consumers[a][ap] == 0 {
                            pools.release(d);
                        }
                        s
                    }
                    None => default_source(scope, kind, pi, p, in_node, &out_dst[in_node]),
                };
                inputs.push(src);
            }
        }
        // outputs (Delay write has none)
        if !(kind == NodeKind::Delay && is_write) {
            for (pi, p) in kind.outputs().iter().enumerate() {
                let implicit = scope == Scope::Voice && ni == in_node && matches!(pi, 2 | 3); // pitch/gate may be read implicitly
                let d = if consumers[ni][pi] > 0 || implicit { pools.acquire(p.ty) } else { Dst::Unused };
                out_dst[ni][pi] = d;
                outputs.push(d);
            }
            // outputs with no consumer (but allocated for implicit reads) stay alive for the whole run
        }
        steps.push(Step { node: ni, kind: if is_write { StepKind::DelayWrite } else { StepKind::Node }, inputs, outputs });
    }
    // Patch implicit defaults that referenced in-node buffers allocated after use order (in node is always first: it has no inputs)
    let sink = steps.iter().find(|s| s.node == out_node).and_then(|s| s.inputs.first().copied()).unwrap_or(Src::Silence);
    let source_dst = out_dst[in_node].clone();
    Ok(CompiledGraph {
        scope,
        nodes: g.nodes.clone(),
        programs,
        steps,
        audio_bufs: pools.audio_n,
        stereo_bufs: pools.stereo_n,
        ctrl_slots: pools.ctrl_n,
        event_bufs: pools.event_n,
        source_dst,
        sink,
        feedback_edges,
        cost,
        order,
    })
}

fn default_source(scope: Scope, kind: NodeKind, pi: usize, p: &Port, _in_node: usize, in_dst: &[Dst]) -> Src {
    if scope == Scope::Voice {
        // implicit voice sources: oscillator pitch and envelope gate
        if kind == NodeKind::Osc
            && pi == 0
            && let Some(Dst::Ctrl(i)) = in_dst.get(2)
        {
            return Src::Ctrl(*i);
        }
        if kind == NodeKind::Env
            && pi == 0
            && let Some(Dst::Event(i)) = in_dst.get(3)
        {
            return Src::Event(*i);
        }
    }
    match (p.ty, p.param) {
        (PortType::Control, Some(i)) => Src::Param(i as u8),
        (PortType::Control, None) => Src::Const(p.default),
        (PortType::Event, _) => Src::Const(1.0),
        _ => Src::Silence,
    }
}

#[derive(Default)]
struct Pools {
    audio_free: Vec<u16>,
    stereo_free: Vec<u16>,
    ctrl_free: Vec<u16>,
    event_free: Vec<u16>,
    audio_n: usize,
    stereo_n: usize,
    ctrl_n: usize,
    event_n: usize,
}

impl Pools {
    fn acquire(&mut self, ty: PortType) -> Dst {
        fn take(free: &mut Vec<u16>, n: &mut usize) -> u16 {
            free.pop().unwrap_or_else(|| {
                *n += 1;
                (*n - 1) as u16
            })
        }
        match ty {
            PortType::Audio => Dst::Audio(take(&mut self.audio_free, &mut self.audio_n)),
            PortType::Stereo => Dst::Stereo(take(&mut self.stereo_free, &mut self.stereo_n)),
            PortType::Control => Dst::Ctrl(take(&mut self.ctrl_free, &mut self.ctrl_n)),
            PortType::Event => Dst::Event(take(&mut self.event_free, &mut self.event_n)),
        }
    }
    fn release(&mut self, d: Dst) {
        match d {
            Dst::Audio(i) => self.audio_free.push(i),
            Dst::Stereo(i) => self.stereo_free.push(i),
            Dst::Ctrl(i) => self.ctrl_free.push(i),
            Dst::Event(i) => self.event_free.push(i),
            Dst::Unused => {}
        }
    }
}

fn reaches(from: usize, to: usize, users: &[Vec<usize>]) -> bool {
    let mut seen = vec![false; users.len()];
    let mut stack = vec![from];
    while let Some(v) = stack.pop() {
        if v == to {
            return true;
        }
        if seen[v] {
            continue;
        }
        seen[v] = true;
        for &u in &users[v] {
            // a Delay write vertex does not propagate to its read vertex (that's the delayed path)
            stack.push(u);
        }
    }
    false
}

/// Find one cycle among `remaining` vertices (depth-first search with a recursion stack).
fn find_cycle(remaining: &[usize], users: &[Vec<usize>]) -> Vec<usize> {
    let in_set = |v: usize| remaining.contains(&v);
    let mut visited = vec![false; users.len()];
    for &start in remaining {
        if visited[start] {
            continue;
        }
        // iterative DFS: (vertex, next user index)
        let mut path: Vec<usize> = vec![start];
        let mut iter: Vec<usize> = vec![0];
        visited[start] = true;
        while let Some(&v) = path.last() {
            let i = *iter.last().unwrap_or(&0);
            let next = users[v].iter().skip(i).find(|&&u| in_set(u)).copied();
            let idx = iter.len() - 1;
            iter[idx] = i + 1 + users[v].iter().skip(i).position(|&u| in_set(u)).unwrap_or(0);
            match next {
                Some(u) => {
                    if let Some(pos) = path.iter().position(|&x| x == u) {
                        let mut cycle = path[pos..].to_vec();
                        cycle.push(u);
                        return cycle;
                    }
                    if !visited[u] {
                        visited[u] = true;
                        path.push(u);
                        iter.push(0);
                    }
                }
                None => {
                    path.pop();
                    iter.pop();
                }
            }
        }
    }
    remaining.to_vec()
}

// ---------------------------------------------------------------------------
// Runtime

/// Scratch buffers shared by all instances of one compiled graph (owned by the synth).
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    audio: Vec<[f32; SUB_BLOCK]>,
    stereo: Vec<[[f32; SUB_BLOCK]; 2]>,
    ctrl: Vec<f32>,
    event: Vec<[f32; SUB_BLOCK]>,
}

impl Workspace {
    pub fn new(g: &CompiledGraph) -> Self {
        Self {
            audio: vec![[0.0; SUB_BLOCK]; g.audio_bufs],
            stereo: vec![[[0.0; SUB_BLOCK]; 2]; g.stereo_bufs],
            ctrl: vec![0.0; g.ctrl_slots],
            event: vec![[0.0; SUB_BLOCK]; g.event_bufs],
        }
    }
}

#[derive(Clone, Debug)]
enum NodeState {
    None,
    Osc(BasicOscillator),
    Noise { rng: DeterministicRng, lp: f32 },
    Env { env: Adsr, prev_gate: f32 },
    Lfo { phase: f64, held: f32, rng: DeterministicRng },
    Math { eval: Evaluator, phase: f64, samples: u64 },
    Filter(Svf),
    Delay { buf: Vec<f32>, head: usize, last: f32 },
    Z1(f32),
    Ramp(f32),
}

/// Per-instance node state (one per voice for the voice graph, one for the global graph).
#[derive(Clone, Debug)]
pub struct GraphState {
    nodes: Vec<NodeState>,
    sample_rate: f32,
}

impl GraphState {
    /// Non-RT: allocate state for every node.
    pub fn new(g: &CompiledGraph, sample_rate: f32, seed: u64) -> Self {
        let max_delay_s = match g.scope {
            Scope::Voice => MAX_DELAY_S_VOICE,
            Scope::Global => MAX_DELAY_S_GLOBAL,
        };
        let nodes = g
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| match n.kind {
                NodeKind::Osc => NodeState::Osc(BasicOscillator::new(f64::from(sample_rate))),
                NodeKind::Noise => NodeState::Noise { rng: DeterministicRng::new(seed ^ 0xA0, i as u64), lp: 0.0 },
                NodeKind::Env => NodeState::Env { env: Adsr::new(sample_rate, env_params(n)), prev_gate: 0.0 },
                NodeKind::Lfo => NodeState::Lfo { phase: 0.0, held: 0.0, rng: DeterministicRng::new(seed ^ 0x1F0, i as u64) },
                NodeKind::Math => NodeState::Math { eval: Evaluator::new(), phase: 0.0, samples: 0 },
                NodeKind::Filter => NodeState::Filter(Svf::new(sample_rate)),
                NodeKind::Delay => {
                    let len = ((max_delay_s * sample_rate) as usize + FEEDBACK_QUANTUM + SUB_BLOCK).next_power_of_two();
                    NodeState::Delay { buf: vec![0.0; len], head: 0, last: 0.0 }
                }
                NodeKind::Z1 => NodeState::Z1(0.0),
                NodeKind::ControlToAudio => NodeState::Ramp(0.0),
                _ => NodeState::None,
            })
            .collect();
        Self { nodes, sample_rate }
    }

    /// RT: clear all state (note start for voices, panic for global).
    pub fn reset(&mut self) {
        for s in &mut self.nodes {
            match s {
                NodeState::Osc(o) => o.reset_phase(0.0),
                NodeState::Noise { lp, .. } => *lp = 0.0,
                NodeState::Env { env, prev_gate } => {
                    env.reset();
                    *prev_gate = 0.0;
                }
                NodeState::Lfo { phase, held, .. } => {
                    *phase = 0.0;
                    *held = 0.0;
                }
                NodeState::Math { eval, phase, samples } => {
                    *eval = Evaluator::new();
                    *phase = 0.0;
                    *samples = 0;
                }
                NodeState::Filter(f) => f.reset(),
                NodeState::Delay { buf, head, last } => {
                    buf.fill(0.0);
                    *head = 0;
                    *last = 0.0;
                }
                NodeState::Z1(v) | NodeState::Ramp(v) => *v = 0.0,
                NodeState::None => {}
            }
        }
    }

    /// Sum of expression diagnostics across nodes (division by zero, domain, non-finite).
    pub fn expr_diagnostics(&self) -> expr::ExprDiagnostics {
        let mut d = expr::ExprDiagnostics::default();
        for s in &self.nodes {
            if let NodeState::Math { eval, .. } = s {
                d.division_by_zero += eval.diagnostics.division_by_zero;
                d.domain_errors += eval.diagnostics.domain_errors;
                d.nonfinite += eval.diagnostics.nonfinite;
            }
        }
        d
    }
}

fn env_params(n: &Node) -> AdsrParams {
    AdsrParams { attack_s: n.param(0), decay_s: n.param(1), sustain: n.param(2), release_s: n.param(3) }.clamped()
}

/// Source values the runtime copies into the in node's buffers.
pub struct Sources<'a> {
    pub mix_l: &'a [f32],
    pub mix_r: &'a [f32],
    /// Normalized MIDI pitch (semitones / 127), voice only.
    pub pitch_norm: f32,
    /// Per-sample gate (1 = held), voice only.
    pub gate: &'a [f32],
    pub velocity: f32,
    pub amp_env: f32,
    pub filter_env: f32,
    pub lfo: [f32; 4],
    pub a4_hz: f32,
    pub tables: &'a BasicTables,
}

impl CompiledGraph {
    /// RT: run one sub-block (`n ≤ SUB_BLOCK` samples). Writes the sink into `out_l/out_r`
    /// (which may alias the mix buffers' content copied beforehand by the caller).
    #[inline(never)]
    pub fn run(&self, state: &mut GraphState, ws: &mut Workspace, src: &Sources<'_>, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(SUB_BLOCK).min(out_r.len());
        // fill sources
        for (pi, d) in self.source_dst.iter().enumerate() {
            match (*d, pi) {
                (Dst::Stereo(i), 0) => {
                    ws.stereo[i as usize][0][..n].copy_from_slice(&src.mix_l[..n]);
                    ws.stereo[i as usize][1][..n].copy_from_slice(&src.mix_r[..n]);
                }
                (Dst::Audio(i), 1) => {
                    let b = &mut ws.audio[i as usize];
                    for ((o, l), r) in b.iter_mut().zip(src.mix_l).zip(src.mix_r).take(n) {
                        *o = 0.5 * (l + r);
                    }
                }
                (Dst::Ctrl(i), p) => {
                    ws.ctrl[i as usize] = match self.scope {
                        Scope::Voice => match p {
                            2 => src.pitch_norm,
                            4 => src.velocity,
                            5 => src.amp_env,
                            6 => src.filter_env,
                            7..=10 => src.lfo[p - 7],
                            _ => 0.0,
                        },
                        Scope::Global => match p {
                            2..=5 => src.lfo[p - 2],
                            _ => 0.0,
                        },
                    }
                }
                (Dst::Event(i), 3) => ws.event[i as usize][..n].copy_from_slice(&src.gate[..n]),
                _ => {}
            }
        }
        for step in &self.steps {
            exec_step(self, step, state, ws, src, n);
        }
        // sink
        match self.sink {
            Src::Stereo(i) => {
                out_l[..n].copy_from_slice(&ws.stereo[i as usize][0][..n]);
                out_r[..n].copy_from_slice(&ws.stereo[i as usize][1][..n]);
            }
            Src::AudioAsStereo(i) => {
                out_l[..n].copy_from_slice(&ws.audio[i as usize][..n]);
                out_r[..n].copy_from_slice(&ws.audio[i as usize][..n]);
            }
            _ => {
                out_l[..n].fill(0.0);
                out_r[..n].fill(0.0);
            }
        }
    }
}

#[inline(always)]
fn ctrl_value(src: Src, ws: &Workspace, node: &Node) -> f32 {
    match src {
        Src::Ctrl(i) => ws.ctrl[i as usize],
        Src::Const(v) => v,
        Src::Param(i) => node.param(i as usize),
        _ => 0.0,
    }
}

/// Copy a mono source into `dst` (silence when unconnected).
#[inline(always)]
fn audio_into(src: Src, ws: &Workspace, dst: &mut [f32; SUB_BLOCK], n: usize) {
    match src {
        Src::Audio(i) => dst[..n].copy_from_slice(&ws.audio[i as usize][..n]),
        _ => dst[..n].fill(0.0),
    }
}

#[allow(clippy::needless_range_loop, reason = "sample loops index several buffers by the same position")]
fn exec_step(g: &CompiledGraph, step: &Step, state: &mut GraphState, ws: &mut Workspace, src: &Sources<'_>, n: usize) {
    let node = &g.nodes[step.node];
    let sr = state.sample_rate;
    let mut tmp = [0.0f32; SUB_BLOCK];
    let mut tmp2 = [0.0f32; SUB_BLOCK];
    macro_rules! out_audio {
        ($idx:expr) => {
            match step.outputs.get($idx) {
                Some(Dst::Audio(i)) => Some(*i as usize),
                _ => None,
            }
        };
    }
    macro_rules! set_ctrl {
        ($idx:expr, $v:expr) => {
            if let Some(Dst::Ctrl(i)) = step.outputs.get($idx) {
                ws.ctrl[*i as usize] = $v;
            }
        };
    }
    match (node.kind, step.kind) {
        (NodeKind::VoiceIn | NodeKind::GlobalIn | NodeKind::VoiceOut | NodeKind::GlobalOut, _) => {}
        (NodeKind::Const, _) => set_ctrl!(0, node.param(0)),
        (NodeKind::Osc, _) => {
            let pitch_norm = ctrl_value(step.inputs[0], ws, node);
            let pmod = ctrl_value(step.inputs[2], ws, node);
            audio_into(step.inputs[1], ws, &mut tmp, n);
            let NodeState::Osc(osc) = &mut state.nodes[step.node] else { return };
            let semis = pitch_norm * 127.0 + node.param(1) + node.param(2) / 100.0 + pmod * 24.0;
            let hz = f64::from(src.a4_hz) * 2f64.powf(f64::from(semis - 69.0) / 12.0);
            osc.set_frequency(hz.clamp(0.0, f64::from(sr) * 0.45));
            osc.set_wave(BasicWave::from_index(node.param(0) as usize));
            osc.set_pulse_width(f64::from(node.param(4)));
            let index = node.param(3);
            let Some(o) = out_audio!(0) else {
                for _ in 0..n {
                    osc.advance();
                }
                return;
            };
            let has_fm = matches!(step.inputs[1], Src::Audio(_));
            for k in 0..n {
                if has_fm {
                    // phase modulation: offset the read phase by the modulator (in cycles)
                    let base = osc.phase();
                    let ph = (base + f64::from(tmp[k] * index) * 0.25).rem_euclid(1.0);
                    osc.reset_phase(ph);
                    ws.audio[o][k] = osc.next(src.tables);
                    osc.reset_phase((base + osc.increment()).rem_euclid(1.0));
                } else {
                    ws.audio[o][k] = osc.next(src.tables);
                }
            }
        }
        (NodeKind::Noise, _) => {
            let NodeState::Noise { rng, lp } = &mut state.nodes[step.node] else { return };
            let colour = node.param(0);
            let level = node.param(1);
            let coeff = 0.02 + colour * 0.98;
            if let Some(o) = out_audio!(0) {
                for k in 0..n {
                    let w = rng.bipolar_f32();
                    *lp += coeff * (w - *lp);
                    ws.audio[o][k] = *lp * level * (1.0 + 2.0 * (1.0 - colour));
                }
            }
        }
        (NodeKind::Env, _) => {
            let gate_src = step.inputs[0];
            let NodeState::Env { env, prev_gate } = &mut state.nodes[step.node] else { return };
            env.set_params(env_params(node));
            let mut level = env.level();
            for k in 0..n {
                let gate = match gate_src {
                    Src::Event(i) => ws.event[i as usize][k],
                    Src::Const(v) => v,
                    _ => 0.0,
                };
                if gate >= 0.5 && *prev_gate < 0.5 {
                    env.note_on();
                } else if gate < 0.5 && *prev_gate >= 0.5 {
                    env.note_off();
                }
                *prev_gate = gate;
                level = env.next();
            }
            set_ctrl!(0, level);
        }
        (NodeKind::Lfo, _) => {
            let rate_mod = ctrl_value(step.inputs[0], ws, node);
            let NodeState::Lfo { phase, held, rng } = &mut state.nodes[step.node] else { return };
            let rate = f64::from(node.param(1)) * 2f64.powf(f64::from(rate_mod) * 4.0);
            let inc = (rate / f64::from(sr)).clamp(0.0, 0.5) * n as f64;
            let before = *phase;
            *phase = (*phase + inc).rem_euclid(1.0);
            let wrapped = *phase < before || inc >= 1.0;
            let p = *phase as f32;
            let v = match node.param(0) as usize {
                0 => (p * std::f32::consts::TAU).sin(),
                1 => 1.0 - 4.0 * (p - 0.5).abs(),
                2 => 1.0 - 2.0 * p,
                3 => {
                    if p < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                _ => {
                    if wrapped || *held == 0.0 {
                        *held = rng.bipolar_f32();
                    }
                    *held
                }
            };
            set_ctrl!(0, (v * node.param(2) + node.param(3)).clamp(-1.0, 1.0));
        }
        (NodeKind::Math, _) => {
            let Some(prog) = g.programs[step.node].as_ref() else { return };
            let mut vars = [0.0f32; expr::VARIABLES.len()];
            for c in 0..4 {
                vars[5 + c] = ctrl_value(step.inputs[4 + c], ws, node);
            }
            vars[11] = sr;
            let pitch_norm = src.pitch_norm;
            let a4 = src.a4_hz;
            let NodeState::Math { eval, phase, samples } = &mut state.nodes[step.node] else { return };
            let hz = a4 * 2f32.powf((pitch_norm * 127.0 - 69.0) / 12.0);
            let inc = f64::from(hz / sr).clamp(0.0, 0.5);
            let ins: [Src; 4] = [step.inputs[0], step.inputs[1], step.inputs[2], step.inputs[3]];
            let o = out_audio!(0);
            for k in 0..n {
                for (j, s) in ins.iter().enumerate() {
                    vars[1 + j] = match s {
                        Src::Audio(i) => ws.audio[*i as usize][k],
                        _ => 0.0,
                    };
                }
                vars[0] = vars[1];
                vars[9] = (*samples as f32) / sr;
                vars[10] = *phase as f32;
                let v = eval.eval(prog, &vars);
                if let Some(o) = o {
                    ws.audio[o][k] = v;
                }
                *samples += 1;
                *phase = (*phase + inc).rem_euclid(1.0);
            }
        }
        (NodeKind::Mix, _) => {
            if let Some(o) = out_audio!(0) {
                tmp[..n].fill(0.0);
                for (j, s) in step.inputs.iter().enumerate().take(4) {
                    if let Src::Audio(i) = s {
                        let gsrc = node.param(j);
                        let b = &ws.audio[*i as usize];
                        for k in 0..n {
                            tmp[k] += b[k] * gsrc;
                        }
                    }
                }
                ws.audio[o][..n].copy_from_slice(&tmp[..n]);
            }
        }
        (NodeKind::StereoMix, _) => {
            if let Some(Dst::Stereo(o)) = step.outputs.first() {
                tmp[..n].fill(0.0);
                tmp2[..n].fill(0.0);
                for (j, s) in step.inputs.iter().enumerate().take(4) {
                    let gsrc = node.param(j);
                    match s {
                        Src::Stereo(i) => {
                            let b = &ws.stereo[*i as usize];
                            for k in 0..n {
                                tmp[k] += b[0][k] * gsrc;
                                tmp2[k] += b[1][k] * gsrc;
                            }
                        }
                        Src::AudioAsStereo(i) => {
                            let b = &ws.audio[*i as usize];
                            for k in 0..n {
                                tmp[k] += b[k] * gsrc;
                                tmp2[k] += b[k] * gsrc;
                            }
                        }
                        _ => {}
                    }
                }
                let o = *o as usize;
                ws.stereo[o][0][..n].copy_from_slice(&tmp[..n]);
                ws.stereo[o][1][..n].copy_from_slice(&tmp2[..n]);
            }
        }
        (NodeKind::Filter, _) => {
            let cmod = ctrl_value(step.inputs[1], ws, node);
            let rmod = ctrl_value(step.inputs[2], ws, node);
            audio_into(step.inputs[0], ws, &mut tmp, n);
            let NodeState::Filter(f) = &mut state.nodes[step.node] else { return };
            f.set_mode(FilterMode::from_index(node.param(0) as usize));
            let cutoff = (node.param(1) * 2f32.powf(cmod * node.param(3))).clamp(10.0, sr * 0.45);
            let res = (node.param(2) * 2f32.powf(rmod * 2.0)).clamp(0.5, 20.0);
            f.set(cutoff, res);
            if let Some(o) = out_audio!(0) {
                for k in 0..n {
                    ws.audio[o][k] = f.process(tmp[k]);
                }
            }
        }
        (NodeKind::Gain, _) => {
            let g_ctrl = ctrl_value(step.inputs[1], ws, node);
            let gain = rawdawg_dsp::math::db_to_lin(node.param(0)) * g_ctrl;
            audio_into(step.inputs[0], ws, &mut tmp, n);
            if let Some(o) = out_audio!(0) {
                for k in 0..n {
                    ws.audio[o][k] = tmp[k] * gain;
                }
            }
        }
        (NodeKind::Delay, StepKind::Node) => {
            // read: at least FEEDBACK_QUANTUM samples behind the head
            let NodeState::Delay { buf, head, .. } = &state.nodes[step.node] else { return };
            let mask = buf.len() - 1;
            let max_s = match g.scope {
                Scope::Voice => MAX_DELAY_S_VOICE,
                Scope::Global => MAX_DELAY_S_GLOBAL,
            };
            let time_s = (node.param(0) * 0.001).clamp(0.0, max_s);
            let delay = (time_s * sr).max(FEEDBACK_QUANTUM as f32).min((buf.len() - SUB_BLOCK - 1) as f32);
            if let Some(o) = out_audio!(0) {
                let d0 = delay.floor() as usize;
                let frac = delay - d0 as f32;
                for k in 0..n {
                    let pos = (*head + k).wrapping_sub(d0);
                    let a = buf[pos & mask];
                    let b = buf[pos.wrapping_sub(1) & mask];
                    ws.audio[o][k] = a + (b - a) * frac;
                }
            }
        }
        (NodeKind::Delay, StepKind::DelayWrite) => {
            let fb_mod = ctrl_value(step.inputs[2], ws, node);
            audio_into(step.inputs[0], ws, &mut tmp, n);
            let NodeState::Delay { buf, head, last } = &mut state.nodes[step.node] else { return };
            let mask = buf.len() - 1;
            let fb = (node.param(1) + fb_mod * 0.5).clamp(0.0, MAX_FEEDBACK);
            let max_s = match g.scope {
                Scope::Voice => MAX_DELAY_S_VOICE,
                Scope::Global => MAX_DELAY_S_GLOBAL,
            };
            let delay = ((node.param(0) * 0.001).clamp(0.0, max_s) * sr).max(FEEDBACK_QUANTUM as f32) as usize;
            for k in 0..n {
                // internal feedback reads the same delayed sample the output produced
                let delayed = buf[(*head + k).wrapping_sub(delay) & mask];
                let v = tmp[k] + fb * fast_tanh(delayed);
                let v = if v.is_finite() { v.clamp(-8.0, 8.0) } else { 0.0 };
                buf[(*head + k) & mask] = v;
                *last = v;
            }
            *head = (*head + n) & mask;
        }
        (NodeKind::Shaper, _) => {
            let dmod = ctrl_value(step.inputs[1], ws, node);
            audio_into(step.inputs[0], ws, &mut tmp, n);
            let drive = (node.param(1) * 2f32.powf(dmod * 2.0)).clamp(1.0, 80.0);
            let level = node.param(2);
            if let Some(o) = out_audio!(0) {
                for k in 0..n {
                    let x = tmp[k] * drive;
                    ws.audio[o][k] = level
                        * match node.param(0) as usize {
                            0 => fast_tanh(x),
                            1 => {
                                // triangle fold into ±1
                                let t = (x + 1.0).rem_euclid(4.0);
                                if t < 2.0 { t - 1.0 } else { 3.0 - t }
                            }
                            2 => x.clamp(-1.0, 1.0),
                            _ => (x.clamp(-8.0, 8.0) * std::f32::consts::FRAC_PI_2).sin(),
                        };
                }
            }
        }
        (NodeKind::Z1, _) => {
            audio_into(step.inputs[0], ws, &mut tmp, n);
            let NodeState::Z1(prev) = &mut state.nodes[step.node] else { return };
            if let Some(o) = out_audio!(0) {
                for k in 0..n {
                    ws.audio[o][k] = *prev;
                    *prev = tmp[k];
                }
            } else if n > 0 {
                *prev = tmp[n - 1];
            }
        }
        (NodeKind::ToStereo, _) => {
            let pmod = ctrl_value(step.inputs[1], ws, node);
            audio_into(step.inputs[0], ws, &mut tmp, n);
            let (gl, gr) = rawdawg_types::units::Pan(f64::from((node.param(0) + pmod).clamp(-1.0, 1.0))).equal_power_gains();
            if let Some(Dst::Stereo(o)) = step.outputs.first() {
                let o = *o as usize;
                for k in 0..n {
                    ws.stereo[o][0][k] = tmp[k] * gl;
                    ws.stereo[o][1][k] = tmp[k] * gr;
                }
            }
        }
        (NodeKind::ToMono, _) => {
            if let Some(o) = out_audio!(0) {
                match step.inputs[0] {
                    Src::Stereo(i) => {
                        let b = &ws.stereo[i as usize];
                        for k in 0..n {
                            tmp[k] = 0.5 * (b[0][k] + b[1][k]);
                        }
                    }
                    Src::AudioAsStereo(i) => tmp[..n].copy_from_slice(&ws.audio[i as usize][..n]),
                    _ => tmp[..n].fill(0.0),
                }
                ws.audio[o][..n].copy_from_slice(&tmp[..n]);
            }
        }
        (NodeKind::ControlToAudio, _) => {
            let target = ctrl_value(step.inputs[0], ws, node) * node.param(0);
            let NodeState::Ramp(cur) = &mut state.nodes[step.node] else { return };
            if let Some(o) = out_audio!(0) {
                let step_v = (target - *cur) / n.max(1) as f32;
                for k in 0..n {
                    *cur += step_v;
                    ws.audio[o][k] = *cur;
                }
            } else {
                *cur = target;
            }
        }
        (NodeKind::AudioToControl, _) => {
            audio_into(step.inputs[0], ws, &mut tmp, n);
            let v = if node.param(0) as usize == 0 { rawdawg_dsp::math::rms(&tmp[..n]) * std::f32::consts::SQRT_2 } else { tmp[n.saturating_sub(1)] };
            set_ctrl!(0, (v * node.param(1)).clamp(-1.0, 1.0));
        }
        (NodeKind::Reduce, _) => {
            let vals: [f32; 4] = std::array::from_fn(|j| ctrl_value(step.inputs[j], ws, node));
            let connected: [bool; 4] = std::array::from_fn(|j| matches!(step.inputs[j], Src::Ctrl(_)));
            let count = connected.iter().filter(|c| **c).count().max(1) as f32;
            let v = match node.param(0) as usize {
                0 => vals.iter().sum(),
                1 => vals.iter().zip(connected).filter(|(_, c)| *c).map(|(v, _)| *v).fold(f32::MIN, f32::max).max(-1.0),
                2 => vals.iter().zip(connected).filter(|(_, c)| *c).map(|(v, _)| *v).sum::<f32>() / count,
                _ => vals.iter().zip(connected).filter(|(_, c)| *c).map(|(v, _)| *v).next_back().unwrap_or(0.0),
            };
            set_ctrl!(0, v.clamp(-1.0, 1.0));
        }
    }
}

// ---------------------------------------------------------------------------
// Mutation / morph helpers (non-RT)

/// Perturb node parameters in their perceptual domain, honouring category locks.
pub fn mutate_params(g: &mut Graph, amount: f32, rng: &mut DeterministicRng, locked: &dyn Fn(ParamCategory) -> bool) {
    for sub in [&mut g.voice, &mut g.global] {
        for node in &mut sub.nodes {
            for (i, spec) in node.kind.params().iter().enumerate() {
                if locked(spec.category) || !rng.chance(f64::from(amount) * 0.6) {
                    continue;
                }
                let cur = node.param(i);
                let v = if !spec.labels.is_empty() {
                    if amount > 0.5 && rng.chance(0.3) { rng.below(spec.labels.len() as u32) as f32 } else { cur }
                } else if spec.log {
                    (cur.max(spec.min) * 2f32.powf(rng.bipolar_f32() * amount * 3.0)).clamp(spec.min, spec.max)
                } else {
                    (cur + rng.bipolar_f32() * amount * (spec.max - spec.min) * 0.3).clamp(spec.min, spec.max)
                };
                if node.params.len() <= i {
                    node.params.resize(spec_len(node.kind), 0.0);
                }
                node.params[i] = v;
            }
        }
    }
}

fn spec_len(kind: NodeKind) -> usize {
    kind.params().len()
}

/// Weighted interpolation of node parameters over graphs with the same topology.
pub fn morph_params(corners: &[&Graph], weights: &[f32], out: &mut Graph) {
    for (scope, sub) in [(Scope::Voice, &mut out.voice), (Scope::Global, &mut out.global)] {
        for (ni, node) in sub.nodes.iter_mut().enumerate() {
            for (pi, spec) in node.kind.params().iter().enumerate() {
                let values: Vec<f32> = corners.iter().map(|c| c.sub(scope).nodes.get(ni).map(|n| n.param(pi)).unwrap_or(spec.default)).collect();
                let v = if !spec.labels.is_empty() {
                    let (mut best, mut bw) = (values[0], -1.0);
                    for (v, w) in values.iter().zip(weights) {
                        if *w > bw {
                            bw = *w;
                            best = *v;
                        }
                    }
                    best
                } else if spec.log {
                    values.iter().zip(weights).map(|(v, w)| v.max(spec.min).ln() * w).sum::<f32>().exp()
                } else {
                    values.iter().zip(weights).map(|(v, w)| v * w).sum()
                };
                if node.params.len() <= pi {
                    node.params.resize(spec_len(node.kind), 0.0);
                }
                node.params[pi] = v.clamp(spec.min, spec.max);
            }
        }
    }
}
