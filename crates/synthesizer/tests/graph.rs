//! T-GRAPH: typed modular graph compiler and runtime (SYN-07).

use rawdawg_dsp::math::{peak, rms};
use rawdawg_dsp::processor::{PrepareSpec, ProcessContext, Processor, TransportView};
use rawdawg_dsp::{AudioBlock, BusLayout};
use rawdawg_rt::alloc_guard::{GuardAllocator, measure_rt};
use rawdawg_synthesizer::Synth;
use rawdawg_synthesizer::graph::{self, Edge, GraphError, GraphNodeId, NodeKind, Scope, SubGraph};
use rawdawg_synthesizer::patch::{Layer, Patch};
use rawdawg_types::events::{EventSource, TimedEvent};

#[global_allocator]
static GLOBAL: GuardAllocator = GuardAllocator;

const SR: f64 = 48_000.0;

struct Harness {
    synth: Synth,
    l: Vec<f32>,
    r: Vec<f32>,
    transport: TransportView,
}

impl Harness {
    fn new(patch: Patch) -> Self {
        let mut synth = Synth::new(patch);
        synth.prepare(&PrepareSpec::new(SR, 2048, BusLayout::Stereo)).unwrap();
        Self { synth, l: vec![0.0; 2048], r: vec![0.0; 2048], transport: TransportView::stopped(SR) }
    }
    fn render(&mut self, total: usize, block: usize, events: &[(usize, TimedEvent)]) -> (Vec<f32>, Vec<f32>) {
        let mut out_l = Vec::with_capacity(total);
        let mut out_r = Vec::with_capacity(total);
        let mut pos = 0;
        let mut evbuf: Vec<TimedEvent> = Vec::new();
        while pos < total {
            let n = block.min(total - pos);
            evbuf.clear();
            for (at, ev) in events {
                if *at >= pos && *at < pos + n {
                    let mut e = *ev;
                    e.frame = (*at - pos) as u32;
                    evbuf.push(e);
                }
            }
            rawdawg_types::events::sort_events(&mut evbuf);
            let mut outs: [&mut [f32]; 2] = [&mut self.l[..n], &mut self.r[..n]];
            let mut ctx = ProcessContext { audio: AudioBlock::output_only(n, &mut outs), events: &evbuf, transport: &self.transport };
            self.synth.process(&mut ctx);
            out_l.extend_from_slice(&self.l[..n]);
            out_r.extend_from_slice(&self.r[..n]);
            pos += n;
        }
        (out_l, out_r)
    }
}

fn on(id: u32, pitch: u8, vel: f32) -> TimedEvent {
    TimedEvent::note_on(0, EventSource::TypingKeyboard, 0, id, pitch, vel)
}
fn off(id: u32, pitch: u8) -> TimedEvent {
    TimedEvent::note_off(0, EventSource::TypingKeyboard, 0, id, pitch)
}
fn edge(from: u32, fp: u8, to: u32, tp: u8) -> Edge {
    Edge { from: GraphNodeId(from), from_port: fp, to: GraphNodeId(to), to_port: tp }
}
fn sine_patch() -> Patch {
    Patch { layers: vec![Layer::basic(rawdawg_dsp::oscillator::BasicWave::Sine)], ..Patch::default() }
}
fn with_voice_graph(mut p: Patch, g: SubGraph) -> Patch {
    p.graph.voice = g;
    p
}
fn with_global_graph(mut p: Patch, g: SubGraph) -> Patch {
    p.graph.global = g;
    p
}

#[test]
fn compiler_rejects_type_mismatch_missing_refs_and_duplicate_inputs() {
    let mut g = SubGraph::passthrough(Scope::Voice);
    g.edges.push(edge(1, 1, 2, 0));
    assert!(matches!(graph::compile(&g, Scope::Voice), Err(GraphError::DuplicateInput { edge: 1 })), "second edge into the same input");
    g.edges.clear();
    let lfo = g.add_node(NodeKind::Lfo, 0.0, 0.0);
    // control output into a stereo input
    g.edges.push(edge(lfo.0, 0, 2, 0));
    let err = graph::compile(&g, Scope::Voice).unwrap_err();
    assert!(matches!(err, GraphError::TypeMismatch { edge: 0, from: "control", to: "stereo" }), "{err}");
    assert_eq!(err.edge(), Some(0));
    // missing node / port
    let mut g = SubGraph::passthrough(Scope::Voice);
    g.edges.push(edge(99, 0, 2, 0));
    assert!(matches!(graph::compile(&g, Scope::Voice), Err(GraphError::MissingNode { .. })));
    let mut g = SubGraph::passthrough(Scope::Voice);
    g.edges[0].from_port = 42;
    assert!(matches!(graph::compile(&g, Scope::Voice), Err(GraphError::MissingPort { .. })));
    // wrong scope and missing in/out
    let g = SubGraph::passthrough(Scope::Global);
    assert!(matches!(graph::compile(&g, Scope::Voice), Err(GraphError::WrongScope(..))));
    let g = SubGraph { nodes: vec![graph::Node::new(GraphNodeId(1), NodeKind::Osc, 0.0, 0.0)], edges: vec![] };
    assert!(matches!(graph::compile(&g, Scope::Voice), Err(GraphError::MissingInOut)));
    // mono audio widens into a stereo input (documented coercion)
    let mut g = SubGraph::passthrough(Scope::Voice);
    g.edges.clear();
    g.edges.push(edge(1, 1, 2, 0));
    assert!(graph::compile(&g, Scope::Voice).is_ok());
}

#[test]
fn instantaneous_cycles_are_rejected_with_path_and_delay_feedback_is_allowed() {
    let mut g = SubGraph::passthrough(Scope::Voice);
    let f = g.add_node(NodeKind::Filter, 0.0, 0.0);
    let gn = g.add_node(NodeKind::Gain, 0.0, 0.0);
    let mix = g.add_node(NodeKind::Mix, 0.0, 0.0);
    g.edges.clear();
    g.edges.push(edge(1, 1, mix.0, 0)); // voice mono → mix.a
    g.edges.push(edge(mix.0, 0, f.0, 0)); // mix → filter
    g.edges.push(edge(f.0, 0, gn.0, 0)); // filter → gain
    g.edges.push(edge(gn.0, 0, mix.0, 1)); // gain → mix.b  (instantaneous loop)
    g.edges.push(edge(f.0, 0, 2, 0)); // filter → out
    let err = graph::compile(&g, Scope::Voice).unwrap_err();
    match &err {
        GraphError::Cycle { path, nodes } => {
            assert!(path.contains("Filter") && path.contains("Gain") && path.contains("Mix"), "{path}");
            assert!(nodes.contains(&f) && nodes.contains(&gn) && nodes.contains(&mix));
        }
        other => panic!("expected cycle error, got {other}"),
    }
    // same loop through a Delay node is explicit delayed feedback
    let d = g.add_node(NodeKind::Delay, 0.0, 0.0);
    g.edges.retain(|e| !(e.from == gn && e.to == mix));
    g.edges.push(edge(gn.0, 0, d.0, 0)); // gain → delay.in (feedback edge)
    g.edges.push(edge(d.0, 0, mix.0, 1)); // delay → mix.b
    let c = graph::compile(&g, Scope::Voice).expect("delayed feedback compiles");
    assert_eq!(c.feedback_edges.len(), 1);
    let fe = g.edges[c.feedback_edges[0]];
    assert_eq!((fe.from, fe.to), (gn, d));
    // the delay read is scheduled before the loop, the write after it
    let read = c.steps.iter().position(|s| c.nodes[s.node].id == d && s.kind == graph::StepKind::Node).unwrap();
    let write = c.steps.iter().position(|s| c.nodes[s.node].id == d && s.kind == graph::StepKind::DelayWrite).unwrap();
    let mixpos = c.steps.iter().position(|s| c.nodes[s.node].id == mix).unwrap();
    assert!(read < mixpos && mixpos < write);
    // a feed-forward delay is not reported as feedback
    let mut g2 = SubGraph::passthrough(Scope::Voice);
    let d2 = g2.add_node(NodeKind::Delay, 0.0, 0.0);
    g2.edges.clear();
    g2.edges.push(edge(1, 1, d2.0, 0));
    g2.edges.push(edge(d2.0, 0, 2, 0));
    assert!(graph::compile(&g2, Scope::Voice).unwrap().feedback_edges.is_empty());
    // a Z1 node is explicit state but not a feedback breaker
    let mut g3 = SubGraph::passthrough(Scope::Voice);
    let z = g3.add_node(NodeKind::Z1, 0.0, 0.0);
    let m = g3.add_node(NodeKind::Mix, 0.0, 0.0);
    g3.edges.clear();
    g3.edges.push(edge(1, 1, m.0, 0));
    g3.edges.push(edge(m.0, 0, z.0, 0));
    g3.edges.push(edge(z.0, 0, m.0, 1));
    g3.edges.push(edge(m.0, 0, 2, 0));
    assert!(matches!(graph::compile(&g3, Scope::Voice), Err(GraphError::Cycle { .. })));
}

#[test]
fn compile_is_deterministic_and_reuses_buffers() {
    let mut g = SubGraph::passthrough(Scope::Voice);
    let ids: Vec<GraphNodeId> = (0..6).map(|_| g.add_node(NodeKind::Gain, 0.0, 0.0)).collect();
    g.edges.clear();
    g.edges.push(edge(1, 1, ids[0].0, 0));
    for w in ids.windows(2) {
        g.edges.push(edge(w[0].0, 0, w[1].0, 0));
    }
    g.edges.push(edge(ids[5].0, 0, 2, 0));
    let a = graph::compile(&g, Scope::Voice).unwrap();
    let b = graph::compile(&g, Scope::Voice).unwrap();
    assert_eq!(a.order, b.order);
    assert_eq!(a.steps.len(), b.steps.len());
    assert!(a.audio_bufs <= 2, "a chain of six gains needs two live mono buffers, got {}", a.audio_bufs);
    // JSON round trip of the model keeps ids/edges
    let json = serde_json::to_string(&g).unwrap();
    let back: SubGraph = serde_json::from_str(&json).unwrap();
    assert_eq!(back, g);
}

#[test]
fn limits_are_enforced_before_compilation() {
    let mut g = SubGraph::passthrough(Scope::Voice);
    for _ in 0..(graph::MAX_NODES - 1) {
        g.add_node(NodeKind::Const, 0.0, 0.0);
    }
    assert!(matches!(graph::compile(&g, Scope::Voice), Err(GraphError::TooManyNodes(_))));
    let mut g = SubGraph::passthrough(Scope::Voice);
    for _ in 0..40 {
        g.add_node(NodeKind::Filter, 0.0, 0.0);
    }
    assert!(matches!(graph::compile(&g, Scope::Voice), Err(GraphError::TooExpensive(..))));
    let mut g = SubGraph::passthrough(Scope::Voice);
    let c = g.add_node(NodeKind::Const, 0.0, 0.0);
    for _ in 0..graph::MAX_EDGES {
        g.edges.push(edge(c.0, 0, 2, 0));
    }
    assert!(matches!(graph::compile(&g, Scope::Voice), Err(GraphError::TooManyEdges(_))));
    // invalid expressions and over-budget programs are rejected through the patch
    let mut g = SubGraph::passthrough(Scope::Voice);
    let m = g.add_node(NodeKind::Math, 0.0, 0.0);
    g.node_mut(m).unwrap().expr = "tanh(x * nope)".into();
    let p = with_voice_graph(sine_patch(), g.clone());
    let e = p.validate().unwrap_err();
    assert!(e.contains("expression") && e.contains("nope"), "{e}");
    g.node_mut(m).unwrap().expr = "x".to_owned() + &" + x".repeat(300);
    let p = with_voice_graph(sine_patch(), g);
    assert!(p.validate().unwrap_err().contains("instructions"));
    assert!(Synth::new(p).set_patch(Patch::default()).is_ok());
}

#[test]
fn passthrough_graph_renders_identically_to_no_graph() {
    let plain = sine_patch();
    let graphed = with_voice_graph(with_global_graph(sine_patch(), SubGraph::passthrough(Scope::Global)), SubGraph::passthrough(Scope::Voice));
    let ev = [(0usize, on(1, 60, 0.9)), (20_000, off(1, 60))];
    let (a, _) = Harness::new(plain).render(40_000, 256, &ev);
    let (b, _) = Harness::new(graphed).render(40_000, 256, &ev);
    assert!(rms(&a) > 0.05);
    let diff = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    assert!(diff < 1e-6, "pass-through graph changes the output by {diff}");
}

#[test]
fn expression_node_scales_the_voice_and_counts_domain_errors() {
    let mut g = SubGraph::passthrough(Scope::Voice);
    let m = g.add_node(NodeKind::Math, 0.0, 0.0);
    g.node_mut(m).unwrap().expr = "x * ctrl1".into();
    g.node_mut(m).unwrap().params[0] = 0.5;
    g.edges.clear();
    g.edges.push(edge(1, 1, m.0, 0));
    g.edges.push(edge(m.0, 0, 2, 0));
    let ev = [(0usize, on(1, 60, 0.9))];
    let (a, _) = Harness::new(sine_patch()).render(24_000, 256, &ev);
    let (b, br) = Harness::new(with_voice_graph(sine_patch(), g.clone())).render(24_000, 256, &ev);
    let ratio = rms(&b) / rms(&a);
    assert!((ratio - 0.5).abs() < 0.02, "expression halves the signal: ratio {ratio}");
    assert!((rms(&b) - rms(&br)).abs() < 1e-5, "mono widened to both channels");
    // invalid domains stay finite and are counted, never NaN
    g.node_mut(m).unwrap().expr = "x / (ctrl1 - 0.5) + log(x)".into();
    let mut h = Harness::new(with_voice_graph(sine_patch(), g));
    let (c, _) = h.render(24_000, 256, &ev);
    assert!(c.iter().all(|v| v.is_finite()));
    assert!(peak(&c) <= 8.0 * 2.0);
    let d = h.synth.expr_diagnostics();
    assert!(d.division_by_zero > 0 && d.domain_errors > 0, "{d:?}");
    assert_eq!(d.nonfinite, 0);
    assert_eq!(h.synth.stats().nonfinite_samples, 0, "synth output stayed finite");
}

#[test]
fn delay_feedback_decays_geometrically_and_stays_bounded() {
    // mix mono → delay(250 ms, fb 0.5) → out in the global graph (after the voice envelope);
    // a short, soft note gives a burst well inside the soft-clip's linear region.
    let mut g = SubGraph::passthrough(Scope::Global);
    let d = g.add_node(NodeKind::Delay, 0.0, 0.0);
    g.node_mut(d).unwrap().params[0] = 250.0;
    g.node_mut(d).unwrap().params[1] = 0.5;
    g.edges.clear();
    g.edges.push(edge(1, 1, d.0, 0));
    g.edges.push(edge(d.0, 0, 2, 0));
    let p = with_global_graph(sine_patch(), g.clone());
    let ev = [(0usize, on(1, 69, 0.25)), (2_000, off(1, 69))];
    let (out, _) = Harness::new(p.clone()).render(96_000, 256, &ev);
    assert!(rms(&out[20_000..24_000]) < 1e-4, "dry burst ended before the first echo");
    let seg = |k: usize| rms(&out[12_000 * k + 1_000..12_000 * k + 2_000]);
    // echoes at 250 ms spacing (12 000 frames); first copy at 12 000, second at 24 000 ...
    let (e1, e2, e3) = (seg(1), seg(2), seg(3));
    assert!(e1 > 0.01, "first echo audible: {e1}");
    assert!((e2 / e1 - 0.5).abs() < 0.08, "feedback 0.5 per repeat: {}", e2 / e1);
    assert!((e3 / e2 - 0.5).abs() < 0.1, "{}", e3 / e2);
    // maximum feedback stays bounded over many repeats (soft clip in the loop)
    g.node_mut(d).unwrap().params[1] = graph::MAX_FEEDBACK;
    g.node_mut(d).unwrap().params[0] = 2.0; // clamps to the 64-sample quantum
    let p2 = with_global_graph(p, g);
    let (out2, _) = Harness::new(p2).render(96_000, 256, &[(0usize, on(1, 69, 1.0)), (2_000, off(1, 69))]);
    assert!(out2.iter().all(|v| v.is_finite()));
    assert!(peak(&out2) < 4.0, "feedback loop bounded: peak {}", peak(&out2));
}

#[test]
fn voice_graph_modulation_is_per_voice_and_global_graph_is_shared() {
    // Envelope → Gain in the voice graph: each voice gets its own envelope (superposition holds).
    let mut vg = SubGraph::passthrough(Scope::Voice);
    let env = vg.add_node(NodeKind::Env, 0.0, 0.0);
    let gn = vg.add_node(NodeKind::Gain, 0.0, 0.0);
    {
        let e = vg.node_mut(env).unwrap();
        e.params[0] = 0.4; // slow attack
        e.params[2] = 1.0;
    }
    vg.edges.clear();
    vg.edges.push(edge(1, 1, gn.0, 0));
    vg.edges.push(edge(env.0, 0, gn.0, 1));
    vg.edges.push(edge(gn.0, 0, 2, 0));
    let mut p = sine_patch();
    p.amp_env.sustain = 1.0;
    p.amp_env.attack_s = 0.001;
    let pv = with_voice_graph(p.clone(), vg);
    let a_only = [(0usize, on(1, 60, 1.0))];
    let b_only = [(7_200usize, on(2, 67, 1.0))];
    let both = [(0usize, on(1, 60, 1.0)), (7_200, on(2, 67, 1.0))];
    let (a, _) = Harness::new(pv.clone()).render(19_200, 256, &a_only);
    let (b, _) = Harness::new(pv.clone()).render(19_200, 256, &b_only);
    let (ab, _) = Harness::new(pv).render(19_200, 256, &both);
    assert!(rms(&ab[9_600..]) > 0.05);
    let err = a.iter().zip(&b).zip(&ab).map(|((x, y), z)| (x + y - z).abs()).fold(0.0f32, f32::max);
    assert!(err < 1e-4, "per-voice graph state: superposition error {err}");

    // Per-voice: the second note's envelope starts at its own note-on, so it begins quietly.
    let early_vs_late = |sig: &[f32]| rms(&sig[7_200..8_400]) / rms(&sig[17_000..19_200]);
    let r_voice = early_vs_late(&b);
    assert!(r_voice < 0.15, "per-voice envelope restarts at note-on: {r_voice}");

    // Same envelope in the global graph: one shared state that started with the render,
    // so the late note is already well into the attack.
    let mut gg = SubGraph::passthrough(Scope::Global);
    let env = gg.add_node(NodeKind::Env, 0.0, 0.0);
    let gn = gg.add_node(NodeKind::Gain, 0.0, 0.0);
    {
        let e = gg.node_mut(env).unwrap();
        e.params[0] = 0.4;
        e.params[2] = 1.0;
    }
    gg.edges.clear();
    gg.edges.push(edge(1, 1, gn.0, 0));
    gg.edges.push(edge(env.0, 0, gn.0, 1));
    gg.edges.push(edge(gn.0, 0, 2, 0));
    let pg = with_global_graph(p, gg);
    let (b, _) = Harness::new(pg).render(19_200, 256, &b_only);
    let r_global = early_vs_late(&b);
    assert!(r_global > 0.3, "global envelope is shared: {r_global}");
}

#[test]
fn oscillator_node_follows_voice_pitch_and_global_lfo_modulates_all_voices() {
    let mut vg = SubGraph::passthrough(Scope::Voice);
    let o = vg.add_node(NodeKind::Osc, 0.0, 0.0);
    vg.node_mut(o).unwrap().params[0] = 0.0; // sine
    vg.edges.clear();
    vg.edges.push(edge(o.0, 0, 2, 0));
    let mut p = with_voice_graph(sine_patch(), vg);
    p.layers[0].level_db = -60.0;
    let (out, _) = Harness::new(p.clone()).render(48_000, 256, &[(0, on(1, 69, 1.0))]);
    let f = rawdawg_dsp::analysis::dominant_frequency(&out[8_000..], SR);
    assert!((f - 440.0).abs() < 1.0, "osc node at voice pitch: {f}");
    // global LFO (square, 2 Hz) gating the mix: alternating loud/silent quarter seconds for every voice
    let mut gg = SubGraph::passthrough(Scope::Global);
    let lfo = gg.add_node(NodeKind::Lfo, 0.0, 0.0);
    let gn = gg.add_node(NodeKind::Gain, 0.0, 0.0);
    {
        let l = gg.node_mut(lfo).unwrap();
        l.params[0] = 3.0; // square
        l.params[1] = 2.0;
        l.params[2] = 0.5;
        l.params[3] = 0.5;
    }
    gg.edges.clear();
    gg.edges.push(edge(1, 1, gn.0, 0));
    gg.edges.push(edge(lfo.0, 0, gn.0, 1));
    gg.edges.push(edge(gn.0, 0, 2, 0));
    let pg = with_global_graph(p, gg);
    let (out, _) = Harness::new(pg).render(48_000, 256, &[(0, on(1, 60, 1.0)), (6_000, on(2, 67, 1.0))]);
    let loud = rms(&out[1_000..11_000]);
    let quiet = rms(&out[13_000..23_000]);
    assert!(loud > 0.05 && quiet < loud * 0.02, "square LFO gates the whole mix: loud {loud} quiet {quiet}");
}

#[test]
fn graph_patch_round_trips_and_older_patches_load_without_graph() {
    let mut g = SubGraph::passthrough(Scope::Voice);
    let m = g.add_node(NodeKind::Math, 10.0, 20.0);
    g.node_mut(m).unwrap().expr = "tanh(x * 3)".into();
    let p = with_voice_graph(sine_patch(), g);
    let json = serde_json::to_string(&p).unwrap();
    let back: Patch = serde_json::from_str(&json).unwrap();
    assert_eq!(back.graph, p.graph);
    assert!(back.validate().is_ok());
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v.as_object_mut().unwrap().remove("graph");
    let old: Patch = serde_json::from_value(v).unwrap();
    assert!(old.graph.is_empty());
    assert_eq!(old.graph.version, graph::GRAPH_VERSION);
    let mut bad = p.clone();
    bad.graph.version = 99;
    assert!(bad.validate().unwrap_err().contains("version"));
}

#[test]
fn migration_carries_graph_state_for_same_topology_and_resets_for_new_topology() {
    let mut g = SubGraph::passthrough(Scope::Voice);
    let d = g.add_node(NodeKind::Delay, 0.0, 0.0);
    g.node_mut(d).unwrap().params[0] = 200.0;
    g.node_mut(d).unwrap().params[1] = 0.6;
    g.edges.clear();
    g.edges.push(edge(1, 1, d.0, 0));
    g.edges.push(edge(d.0, 0, 2, 0));
    let mut p = with_voice_graph(sine_patch(), g.clone());
    p.amp_env.sustain = 1.0;
    let mut h = Harness::new(p.clone());
    h.render(24_000, 256, &[(0, on(1, 60, 1.0))]);
    // same topology, different parameter: delay contents survive the swap
    let mut p2 = p.clone();
    p2.graph.voice.node_mut(d).unwrap().params[1] = 0.3;
    let mut new = Synth::new(p2);
    new.prepare(&PrepareSpec::new(SR, 2048, BusLayout::Stereo)).unwrap();
    new.migrate_from(&mut h.synth);
    let mut h2 = Harness { synth: new, l: vec![0.0; 2048], r: vec![0.0; 2048], transport: TransportView::stopped(SR) };
    let (cont, _) = h2.render(1024, 256, &[]);
    assert!(rms(&cont) > 0.05, "delayed signal keeps flowing right after the swap (state carried): {}", rms(&cont));
    // new topology: states are fresh but the voice keeps sounding (no hanging/cut note)
    let mut g3 = g.clone();
    let gn = g3.add_node(NodeKind::Gain, 0.0, 0.0);
    g3.edges.retain(|e| e.to != GraphNodeId(2));
    g3.edges.push(edge(d.0, 0, gn.0, 0));
    g3.edges.push(edge(gn.0, 0, 2, 0));
    let mut new3 = Synth::new(with_voice_graph(p, g3));
    new3.prepare(&PrepareSpec::new(SR, 2048, BusLayout::Stereo)).unwrap();
    new3.migrate_from(&mut h2.synth);
    let mut h3 = Harness { synth: new3, l: vec![0.0; 2048], r: vec![0.0; 2048], transport: TransportView::stopped(SR) };
    assert_eq!(h3.synth.stats().active_voices, 1);
    let (first, _) = h3.render(4_000, 256, &[]);
    assert!(rms(&first[..2_000]) < 1e-3, "fresh delay state starts empty");
    let (later, _) = h3.render(12_000, 256, &[]);
    assert!(rms(&later) > 0.05, "the live voice feeds the new delay");
    h3.render(48_000, 256, &[(0, off(1, 60))]);
    assert_eq!(h3.synth.stats().active_voices, 0);
}

#[test]
fn graph_process_path_is_allocation_free() {
    let mut vg = SubGraph::passthrough(Scope::Voice);
    let o = vg.add_node(NodeKind::Osc, 0.0, 0.0);
    let m = vg.add_node(NodeKind::Math, 0.0, 0.0);
    let f = vg.add_node(NodeKind::Filter, 0.0, 0.0);
    let d = vg.add_node(NodeKind::Delay, 0.0, 0.0);
    let mix = vg.add_node(NodeKind::Mix, 0.0, 0.0);
    let env = vg.add_node(NodeKind::Env, 0.0, 0.0);
    let lfo = vg.add_node(NodeKind::Lfo, 0.0, 0.0);
    let st = vg.add_node(NodeKind::ToStereo, 0.0, 0.0);
    let nz = vg.add_node(NodeKind::Noise, 0.0, 0.0);
    let a2c = vg.add_node(NodeKind::AudioToControl, 0.0, 0.0);
    let red = vg.add_node(NodeKind::Reduce, 0.0, 0.0);
    vg.node_mut(m).unwrap().expr = "tanh(x * ctrl1 * 4) + in2 * 0.1".into();
    vg.edges.clear();
    vg.edges.extend([
        edge(1, 1, mix.0, 0),
        edge(o.0, 0, mix.0, 1),
        edge(nz.0, 0, m.0, 1),
        edge(mix.0, 0, m.0, 0),
        edge(env.0, 0, m.0, 4),
        edge(m.0, 0, f.0, 0),
        edge(lfo.0, 0, red.0, 0),
        edge(a2c.0, 0, red.0, 1),
        edge(red.0, 0, f.0, 1),
        edge(f.0, 0, d.0, 0),
        edge(d.0, 0, mix.0, 2),
        edge(mix.0, 0, a2c.0, 0),
        edge(f.0, 0, st.0, 0),
        edge(lfo.0, 0, st.0, 1),
        edge(st.0, 0, 2, 0),
    ]);
    let mut gg = SubGraph::passthrough(Scope::Global);
    let sh = gg.add_node(NodeKind::Shaper, 0.0, 0.0);
    let z = gg.add_node(NodeKind::Z1, 0.0, 0.0);
    let c2a = gg.add_node(NodeKind::ControlToAudio, 0.0, 0.0);
    let cst = gg.add_node(NodeKind::Const, 0.0, 0.0);
    let smix = gg.add_node(NodeKind::StereoMix, 0.0, 0.0);
    let mono = gg.add_node(NodeKind::ToMono, 0.0, 0.0);
    gg.edges.clear();
    gg.edges.extend([
        edge(1, 0, mono.0, 0),
        edge(mono.0, 0, sh.0, 0),
        edge(sh.0, 0, z.0, 0),
        edge(cst.0, 0, c2a.0, 0),
        edge(z.0, 0, smix.0, 0),
        edge(c2a.0, 0, smix.0, 1),
        edge(1, 0, smix.0, 2),
        edge(smix.0, 0, 2, 0),
    ]);
    let mut p = with_global_graph(with_voice_graph(sine_patch(), vg), gg);
    p.max_voices = 8;
    let mut h = Harness::new(p);
    h.render(8_192, 256, &[(0, on(1, 60, 1.0)), (0, on(2, 64, 0.7))]);
    let mut events: Vec<TimedEvent> = (0..6u32).map(|i| on(10 + i, 48 + i as u8, 0.8)).collect();
    events.push(off(1, 60));
    let mut frames_list = [64usize, 256, 1024, 2048].iter().cycle();
    let (_, allocs) = measure_rt(|| {
        for _ in 0..32 {
            let n = *frames_list.next().unwrap();
            for ev in events.iter_mut() {
                ev.frame = 0;
            }
            rawdawg_types::events::sort_events(&mut events);
            let mut outs: [&mut [f32]; 2] = [&mut h.l[..n], &mut h.r[..n]];
            let mut ctx = ProcessContext { audio: AudioBlock::output_only(n, &mut outs), events: &events, transport: &h.transport };
            h.synth.process(&mut ctx);
            h.synth.reset();
        }
    });
    assert_eq!(allocs, 0, "allocations observed on the graph process path");
    let (out, _) = h.render(4_096, 256, &[(0, on(1, 60, 1.0))]);
    assert!(out.iter().all(|v| v.is_finite()));
}

#[test]
fn mutation_keeps_graph_topology_and_morph_requires_it() {
    use rawdawg_synthesizer::mutate::mutate_once;
    use rawdawg_synthesizer::patch::Locks;
    let mut g = SubGraph::passthrough(Scope::Voice);
    let f = g.add_node(NodeKind::Filter, 0.0, 0.0);
    g.edges.clear();
    g.edges.push(edge(1, 1, f.0, 0));
    g.edges.push(edge(f.0, 0, 2, 0));
    let p = with_voice_graph(sine_patch(), g);
    let mut locks = Locks::default();
    let child = mutate_once(&p, 7, 90.0, &locks);
    assert!(child.graph.same_topology(&p.graph));
    assert!(child.validate().is_ok());
    let changed = (0..200u64).filter(|s| mutate_once(&p, *s, 90.0, &locks).graph != p.graph).count();
    assert!(changed > 100, "graph node parameters take part in mutation ({changed}/200)");
    locks.set_category(rawdawg_types::params::ParamCategory::Filter, true);
    assert!((0..50u64).all(|s| mutate_once(&p, s, 90.0, &locks).graph.voice.node(f).unwrap().params == p.graph.voice.node(f).unwrap().params), "locked category keeps node params");
    // morph: identical topology interpolates, different topology is incompatible
    let mut q = p.clone();
    q.graph.voice.node_mut(f).unwrap().params[1] = 8000.0;
    let mid = rawdawg_synthesizer::morph::morph(&[&p, &q, &p, &q], 0.5, 0.5).unwrap();
    let c = mid.graph.voice.node(f).unwrap().params[1];
    assert!(c > 2000.0 && c < 8000.0, "log-interpolated cutoff {c}");
    let plain = sine_patch();
    assert!(rawdawg_synthesizer::morph::morph(&[&p, &plain, &p, &p], 0.5, 0.5).is_none());
}
