//! Advanced modular graph editor (SYN-07): typed ports, drag-to-connect, node
//! palette with search, pan/zoom, error highlighting and the last playable graph.
//!
//! Edits go to a draft; a draft that compiles is committed to the patch at once
//! (audible, undoable). An invalid draft stays on screen with the error while the
//! last valid graph keeps playing.

use super::App;
use crate::theme;
use egui::{Color32, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use rawdawg_synthesizer::expr;
use rawdawg_synthesizer::graph::{self, Edge, Graph, GraphError, GraphNodeId, NodeKind, PortType, Scope, SubGraph};
use rawdawg_types::InstanceId;

const NODE_W: f32 = 170.0;
const HEADER_H: f32 = 22.0;
const PORT_H: f32 = 18.0;
const PARAM_H: f32 = 26.0;
const PORT_R: f32 = 5.0;

pub(crate) struct GraphEditorState {
    pub scope: Scope,
    pan: Vec2,
    zoom: f32,
    selected: Option<GraphNodeId>,
    /// Edge being dragged out of an output port.
    drag_from: Option<(GraphNodeId, u8)>,
    /// Node being moved: id and its current on-screen position (world units) until release.
    moving: Option<(GraphNodeId, Vec2)>,
    /// Invalid draft kept on screen with its error (the live graph keeps playing).
    draft: Option<(InstanceId, Graph, GraphError)>,
    palette_open: bool,
    search: String,
    /// Expression text being edited (applied on Enter / focus loss).
    expr_edit: Option<(GraphNodeId, String)>,
    /// Node parameter being dragged: (node, param index, value) committed on release.
    param_drag: Option<(GraphNodeId, usize, f32)>,
    /// Frames left during which the synth panel scrolls the editor into view (`--ui-preset graph`).
    pub focus_frames: u8,
}

impl Default for GraphEditorState {
    fn default() -> Self {
        Self {
            scope: Scope::Voice,
            pan: vec2(0.0, 0.0),
            zoom: 1.0,
            selected: None,
            drag_from: None,
            moving: None,
            draft: None,
            palette_open: false,
            search: String::new(),
            expr_edit: None,
            param_drag: None,
            focus_frames: 0,
        }
    }
}

fn port_color(ty: PortType) -> Color32 {
    match ty {
        PortType::Audio => Color32::from_rgb(0x6F, 0xC9, 0xB8),
        PortType::Stereo => Color32::from_rgb(0x7F, 0xA8, 0xE6),
        PortType::Control => Color32::from_rgb(0xE6, 0xC9, 0x6E),
        PortType::Event => Color32::from_rgb(0xD9, 0x8A, 0xE0),
    }
}

fn node_height(kind: NodeKind) -> f32 {
    let rows = kind.inputs().len().max(kind.outputs().len()) as f32;
    let params = kind.params().len() as f32 + if kind == NodeKind::Math { 2.0 } else { 0.0 };
    HEADER_H + rows * PORT_H + params * PARAM_H + 8.0
}

/// Try to make `graph` the live graph: commits when it compiles, otherwise keeps a draft.
fn try_commit(app: &mut App, instance_id: InstanceId, mut patch: rawdawg_synthesizer::Patch, graph: Graph) {
    match graph.validate() {
        Ok(()) => {
            patch.graph = graph;
            app.synth_panel.graph.draft = None;
            super::synth_panel::commit_patch(app, instance_id, patch);
        }
        Err(e) => {
            app.status_line = format!("Graph not applied: {e}");
            app.synth_panel.graph.draft = Some((instance_id, graph, e));
        }
    }
}

pub(crate) fn show(app: &mut App, ui: &mut egui::Ui, instance_id: InstanceId, patch: &rawdawg_synthesizer::Patch) {
    // the graph shown: draft (if invalid) or the live one
    let (mut graph, error): (Graph, Option<GraphError>) = match &app.synth_panel.graph.draft {
        Some((id, g, e)) if *id == instance_id => (g.clone(), Some(e.clone())),
        _ => (patch.graph.clone(), None),
    };
    let mut changed = false;
    ui.horizontal(|ui| {
        let st = &mut app.synth_panel.graph;
        for (s, label, tip) in [
            (Scope::Voice, "Voice graph", "Runs once per voice between the layers and the filter/amp stage"),
            (Scope::Global, "Global graph", "Runs once on the summed voices before the master gain"),
        ] {
            if ui.selectable_label(st.scope == s, label).on_hover_text(tip).clicked() {
                st.scope = s;
                st.selected = None;
                st.drag_from = None;
            }
        }
        ui.separator();
        let scope = st.scope;
        let sub_empty = graph.sub(scope).is_empty();
        if sub_empty {
            if ui.button("Start graph").on_hover_text("Adds the fixed in/out nodes connected straight through (no change in sound)").clicked() {
                *graph.sub_mut(scope) = SubGraph::passthrough(scope);
                changed = true;
            }
            ui.label(egui::RichText::new("Off — the sound passes straight through.").color(theme::TEXT_SECONDARY));
        } else {
            if ui.button("+ Add node").clicked() {
                st.palette_open = !st.palette_open;
            }
            if ui.button("Remove graph").on_hover_text("Back to pass-through (undoable)").clicked() {
                *graph.sub_mut(scope) = SubGraph::default();
                st.selected = None;
                changed = true;
            }
            if ui.button("Reset view").clicked() {
                st.pan = vec2(0.0, 0.0);
                st.zoom = 1.0;
            }
            if let Some(sel) = st.selected
                && graph.sub(scope).node(sel).is_some_and(|n| !n.kind.is_fixed())
                && ui.button("Delete selected").clicked()
            {
                graph.sub_mut(scope).remove_node(sel);
                st.selected = None;
                changed = true;
            }
        }
    });
    let scope = app.synth_panel.graph.scope;
    if graph.sub(scope).is_empty() {
        if changed {
            try_commit(app, instance_id, patch.clone(), graph);
        }
        return;
    }
    // palette
    if app.synth_panel.graph.palette_open {
        egui::Frame::group(ui.style()).fill(theme::PANEL).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Find node");
                let r = ui.add(egui::TextEdit::singleline(&mut app.synth_panel.graph.search).desired_width(160.0).hint_text("filter, delay, expression…"));
                if ui.button("Close").clicked() {
                    app.synth_panel.graph.palette_open = false;
                }
                r.request_focus();
            });
            let needle = app.synth_panel.graph.search.to_lowercase();
            ui.horizontal_wrapped(|ui| {
                for kind in NodeKind::USER {
                    if !needle.is_empty() && !kind.label().to_lowercase().contains(&needle) && !kind.description().to_lowercase().contains(&needle) {
                        continue;
                    }
                    if ui.button(kind.label()).on_hover_text(kind.description()).clicked() {
                        let st = &app.synth_panel.graph;
                        let world = (vec2(240.0, 40.0) - st.pan) / st.zoom;
                        let id = graph.sub_mut(scope).add_node(kind, world.x, world.y);
                        app.synth_panel.graph.selected = Some(id);
                        app.synth_panel.graph.palette_open = false;
                        changed = true;
                    }
                }
            });
        });
    }
    // error / status line
    match &error {
        Some(e) => {
            ui.colored_label(theme::ERROR, format!("Not applied: {e}  (the last valid graph keeps playing)"));
        }
        None => {
            let budget = match scope {
                Scope::Voice => graph::MAX_VOICE_COST,
                Scope::Global => graph::MAX_GLOBAL_COST,
            };
            let d = app.synth_mirrors.get(&instance_id).map(|s| s.expr_diagnostics()).unwrap_or_default();
            let mut txt = match graph::compile(graph.sub(scope), scope) {
                Ok(c) => format!(
                    "Playing · {} nodes · cost {}/{}{}",
                    c.nodes.len(),
                    c.cost,
                    budget,
                    if c.feedback_edges.is_empty() { String::new() } else { format!(" · {} feedback edge(s)", c.feedback_edges.len()) }
                ),
                Err(e) => format!("Invalid: {e}"),
            };
            if d.division_by_zero + d.domain_errors + d.nonfinite > 0 {
                txt += &format!("  · expression guards hit: {} ÷0, {} domain, {} non-finite", d.division_by_zero, d.domain_errors, d.nonfinite);
            }
            ui.label(egui::RichText::new(txt).color(theme::TEXT_SECONDARY));
        }
    }
    ui.label(
        egui::RichText::new(
            "Drag from an output dot to an input dot to connect · drag node titles to move · right-click a node or an edge handle to delete · wheel/Ctrl+wheel: pan/zoom",
        )
        .color(theme::TEXT_SECONDARY)
        .small(),
    );

    // canvas
    // fill the visible part of the editor panel (the panel is resizable)
    let height = (ui.clip_rect().bottom() - ui.cursor().top() - 12.0).clamp(300.0, 640.0);
    let (rect, bg) = ui.allocate_exact_size(vec2(ui.available_width().max(400.0), height), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::BACKGROUND);
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::BORDER), egui::StrokeKind::Inside);
    // pan / zoom
    if bg.dragged() {
        app.synth_panel.graph.pan += bg.drag_delta();
    }
    if bg.clicked() {
        app.synth_panel.graph.selected = None;
    }
    if bg.hovered() || ui.rect_contains_pointer(rect) {
        let (scroll, zoom, ctrl) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.command));
        if zoom != 1.0 {
            let st = &mut app.synth_panel.graph;
            let old = st.zoom;
            st.zoom = (st.zoom * zoom).clamp(0.4, 2.5);
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                let local = p - rect.min;
                st.pan = local - (local - st.pan) * (st.zoom / old);
            }
        } else if scroll != Vec2::ZERO && !ctrl {
            app.synth_panel.graph.pan += scroll;
        }
    }
    let (pan, zoom) = (app.synth_panel.graph.pan, app.synth_panel.graph.zoom);
    let to_screen = |w: Pos2| rect.min + pan + w.to_vec2() * zoom;
    let sub = graph.sub(scope).clone();
    let feedback: Vec<usize> = if error.is_none() { graph::compile(&sub, scope).map(|c| c.feedback_edges).unwrap_or_default() } else { Vec::new() };
    let bad_edge = error.as_ref().and_then(|e| e.edge());
    let bad_nodes: Vec<GraphNodeId> = error.as_ref().map(|e| e.nodes().to_vec()).unwrap_or_default();

    // port positions (a node being dragged follows the pointer until release)
    let moving = app.synth_panel.graph.moving;
    let node_pos = move |n: &graph::Node| match moving {
        Some((id, p)) if id == n.id => pos2(p.x, p.y),
        _ => pos2(n.x, n.y),
    };
    let node_rect = move |n: &graph::Node| Rect::from_min_size(to_screen(node_pos(n)), vec2(NODE_W, node_height(n.kind)) * zoom);
    let in_port_pos = |n: &graph::Node, i: usize| {
        let r = node_rect(n);
        pos2(r.min.x, r.min.y + (HEADER_H + PORT_H * (i as f32 + 0.5)) * zoom)
    };
    let out_port_pos = |n: &graph::Node, i: usize| {
        let r = node_rect(n);
        pos2(r.max.x, r.min.y + (HEADER_H + PORT_H * (i as f32 + 0.5)) * zoom)
    };
    let bezier = |a: Pos2, b: Pos2, stroke: Stroke| {
        let d = ((b.x - a.x).abs() * 0.5).max(30.0 * zoom);
        egui::Shape::CubicBezier(egui::epaint::CubicBezierShape::from_points_stroke([a, pos2(a.x + d, a.y), pos2(b.x - d, b.y), b], false, Color32::TRANSPARENT, stroke))
    };

    // edges
    let mut delete_edge: Option<usize> = None;
    for (ei, e) in sub.edges.iter().enumerate() {
        let (Some(a), Some(b)) = (sub.node(e.from), sub.node(e.to)) else { continue };
        let pa = out_port_pos(a, e.from_port as usize);
        let pb = in_port_pos(b, e.to_port as usize);
        let ty = a.kind.outputs().get(e.from_port as usize).map(|p| p.ty).unwrap_or(PortType::Audio);
        let mut color = port_color(ty);
        if feedback.contains(&ei) {
            color = theme::WARNING;
        }
        if bad_edge == Some(ei) {
            color = theme::ERROR;
        }
        painter.add(bezier(pa, pb, Stroke::new(2.0 * zoom.max(0.6), color)));
        // midpoint handle for deletion
        let mid = pos2((pa.x + pb.x) * 0.5, (pa.y + pb.y) * 0.5);
        let hr = Rect::from_center_size(mid, vec2(10.0, 10.0));
        let resp = ui.interact(hr, ui.scope_id().with(("edge", ei)), Sense::click());
        painter.circle_filled(mid, if resp.hovered() { 5.0 } else { 3.0 }, color);
        if resp.secondary_clicked() || (resp.clicked() && ui.input(|i| i.modifiers.alt)) {
            delete_edge = Some(ei);
        }
        resp.on_hover_text(if feedback.contains(&ei) { "Feedback edge (through the Delay node). Right-click to delete." } else { "Right-click to delete this connection" });
    }

    // nodes
    let mut pending_connect: Option<Edge> = None;
    let mut delete_node: Option<GraphNodeId> = None;
    let mut param_commit: Option<(GraphNodeId, usize, f32)> = None;
    let mut expr_commit: Option<(GraphNodeId, String)> = None;
    let mut moved: Option<(GraphNodeId, f32, f32)> = None;
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let released = ui.input(|i| i.pointer.any_released());
    let mut hovered_input: Option<(GraphNodeId, u8)> = None;
    for n in &sub.nodes {
        let r = node_rect(n);
        if !rect.intersects(r) {
            continue;
        }
        let selected = app.synth_panel.graph.selected == Some(n.id);
        let bad = bad_nodes.contains(&n.id);
        let stroke = if bad {
            Stroke::new(2.0, theme::ERROR)
        } else if selected {
            Stroke::new(2.0, theme::ACCENT)
        } else {
            Stroke::new(1.0, theme::BORDER)
        };
        painter.rect(r, 5.0, theme::PANEL, stroke, egui::StrokeKind::Inside);
        let header = Rect::from_min_size(r.min, vec2(r.width(), HEADER_H * zoom));
        painter.rect_filled(header, 5.0, if n.kind.is_fixed() { theme::RAISED } else { theme::RAISED.linear_multiply(0.8) });
        painter.text(header.left_center() + vec2(8.0 * zoom, 0.0), egui::Align2::LEFT_CENTER, n.kind.label(), egui::FontId::proportional(13.0 * zoom), theme::TEXT);
        // header drag / select / context
        let hresp = ui.interact(header, ui.scope_id().with(("node", n.id)), Sense::click_and_drag());
        if hresp.drag_started() {
            app.synth_panel.graph.moving = Some((n.id, vec2(n.x, n.y)));
            app.synth_panel.graph.selected = Some(n.id);
        }
        if hresp.clicked() {
            app.synth_panel.graph.selected = Some(n.id);
        }
        if hresp.dragged()
            && let Some((id, p)) = app.synth_panel.graph.moving
            && id == n.id
        {
            let d = hresp.drag_delta() / zoom;
            app.synth_panel.graph.moving = Some((id, p + d));
        }
        if hresp.drag_stopped()
            && let Some((id, p)) = app.synth_panel.graph.moving.take()
            && id == n.id
        {
            moved = Some((id, p.x, p.y));
        }
        if !n.kind.is_fixed() {
            hresp.context_menu(|ui| {
                if ui.button("Delete node").clicked() {
                    delete_node = Some(n.id);
                    ui.close();
                }
            });
        }
        hresp.on_hover_text(n.kind.description());
        // ports
        for (i, p) in n.kind.inputs().iter().enumerate() {
            let pos = in_port_pos(n, i);
            let pr = Rect::from_center_size(pos, vec2(14.0, 14.0));
            let resp = ui.interact(pr, ui.scope_id().with(("in", n.id, i)), Sense::click());
            let connected = sub.edges.iter().any(|e| e.to == n.id && usize::from(e.to_port) == i);
            let col = port_color(p.ty);
            if connected {
                painter.circle_filled(pos, PORT_R * zoom, col);
            } else {
                painter.circle_stroke(pos, PORT_R * zoom, Stroke::new(1.5, col));
            }
            if resp.hovered() {
                hovered_input = Some((n.id, i as u8));
                painter.circle_stroke(pos, (PORT_R + 3.0) * zoom, Stroke::new(1.0, theme::TEXT));
            }
            let implicit = scope == Scope::Voice && !connected && ((n.kind == NodeKind::Osc && i == 0) || (n.kind == NodeKind::Env && i == 0));
            let label = if implicit { format!("{} (voice)", p.name) } else { p.name.to_owned() };
            painter.text(pos + vec2(10.0 * zoom, 0.0), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(11.0 * zoom), theme::TEXT_SECONDARY);
            resp.on_hover_text(format!("{} input", p.ty.label()));
        }
        for (i, p) in n.kind.outputs().iter().enumerate() {
            let pos = out_port_pos(n, i);
            let pr = Rect::from_center_size(pos, vec2(14.0, 14.0));
            let resp = ui.interact(pr, ui.scope_id().with(("out", n.id, i)), Sense::click_and_drag());
            let col = port_color(p.ty);
            painter.circle_filled(pos, PORT_R * zoom, col);
            painter.text(pos - vec2(10.0 * zoom, 0.0), egui::Align2::RIGHT_CENTER, p.name, egui::FontId::proportional(11.0 * zoom), theme::TEXT_SECONDARY);
            if resp.drag_started() {
                app.synth_panel.graph.drag_from = Some((n.id, i as u8));
            }
            resp.on_hover_text(format!("{} output — drag to an input", p.ty.label()));
        }
        // parameters
        let body_top = r.min.y + (HEADER_H + PORT_H * n.kind.inputs().len().max(n.kind.outputs().len()) as f32) * zoom;
        let body = Rect::from_min_max(pos2(r.min.x + 6.0 * zoom, body_top), pos2(r.max.x - 6.0 * zoom, r.max.y - 4.0 * zoom));
        if zoom >= 0.7 && !n.kind.params().is_empty() || n.kind == NodeKind::Math {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
            child.set_clip_rect(body.intersect(rect));
            child.set_max_width(body.width());
            child.spacing_mut().item_spacing = vec2(4.0, 2.0);
            child.style_mut().override_font_id = Some(egui::FontId::proportional(11.0 * zoom));
            if n.kind == NodeKind::Math {
                let editing = match &app.synth_panel.graph.expr_edit {
                    Some((id, s)) if *id == n.id => s.clone(),
                    _ => n.expr.clone(),
                };
                let mut text = editing;
                let te = child.add(egui::TextEdit::singleline(&mut text).desired_width(body.width() - 16.0).font(egui::FontId::monospace(11.0 * zoom)));
                if te.changed() {
                    app.synth_panel.graph.expr_edit = Some((n.id, text.clone()));
                }
                let enter = te.lost_focus() && child.input(|i| i.key_pressed(egui::Key::Enter));
                if (te.lost_focus() || enter)
                    && let Some((id, s)) = app.synth_panel.graph.expr_edit.take()
                    && id == n.id
                    && s != n.expr
                {
                    expr_commit = Some((n.id, s));
                }
                child.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(("expr-example", n.id)).selected_text("Examples").width(80.0).show_ui(ui, |ui| {
                        for (name, src) in expr::EXAMPLES {
                            if ui.selectable_label(false, format!("{name}: {src}")).clicked() {
                                expr_commit = Some((n.id, src.to_owned()));
                            }
                        }
                    });
                    match expr::compile(&text) {
                        Ok(p) => ui.label(egui::RichText::new(format!("{} ops", p.ops.len())).color(theme::TEXT_SECONDARY)),
                        Err(e) => ui.colored_label(theme::ERROR, e.to_string()),
                    };
                });
            }
            for (pi, spec) in n.kind.params().iter().enumerate() {
                let live = match app.synth_panel.graph.param_drag {
                    Some((id, i, v)) if id == n.id && i == pi => v,
                    _ => n.param(pi),
                };
                child.horizontal(|ui| {
                    ui.label(spec.name);
                    if !spec.labels.is_empty() {
                        let mut idx = live as usize;
                        egui::ComboBox::from_id_salt(("gp", n.id, pi)).selected_text(spec.labels.get(idx).copied().unwrap_or("?")).width(70.0).show_ui(ui, |ui| {
                            for (k, l) in spec.labels.iter().enumerate() {
                                ui.selectable_value(&mut idx, k, *l);
                            }
                        });
                        if idx != live as usize {
                            param_commit = Some((n.id, pi, idx as f32));
                        }
                    } else {
                        let mut v = live;
                        let speed = if spec.log { (v.abs().max(spec.min.abs()) * 0.01).max(1e-4) } else { (spec.max - spec.min) * 0.004 };
                        let dv = ui.add(egui::DragValue::new(&mut v).range(spec.min..=spec.max).speed(speed).max_decimals(3));
                        if dv.changed() {
                            app.synth_panel.graph.param_drag = Some((n.id, pi, v));
                        }
                        if dv.drag_stopped() || (dv.lost_focus() && dv.changed()) || (dv.changed() && !dv.dragged()) {
                            if let Some((id, i, val)) = app.synth_panel.graph.param_drag.take()
                                && id == n.id
                                && i == pi
                            {
                                param_commit = Some((n.id, pi, val));
                            } else if dv.changed() {
                                param_commit = Some((n.id, pi, v));
                            }
                        }
                    }
                });
            }
        }
    }
    // edge being dragged
    if let Some((from, port)) = app.synth_panel.graph.drag_from {
        if let (Some(n), Some(p)) = (sub.node(from), pointer) {
            let a = out_port_pos(n, port as usize);
            let ty = n.kind.outputs().get(port as usize).map(|x| x.ty).unwrap_or(PortType::Audio);
            painter.add(bezier(a, p, Stroke::new(2.0, port_color(ty))));
        }
        if released {
            if let Some((to, tp)) = hovered_input
                && to != from
            {
                pending_connect = Some(Edge { from, from_port: port, to, to_port: tp });
            }
            app.synth_panel.graph.drag_from = None;
        }
    }

    // apply edits (a node move is a cosmetic patch change: committed on release, voices carry over)
    if let Some((id, x, y)) = moved
        && let Some(n) = graph.sub_mut(scope).node_mut(id)
    {
        n.x = x;
        n.y = y;
        changed = true;
    }
    if let Some(ei) = delete_edge {
        graph.sub_mut(scope).edges.remove(ei);
        changed = true;
    }
    if let Some(id) = delete_node {
        graph.sub_mut(scope).remove_node(id);
        if app.synth_panel.graph.selected == Some(id) {
            app.synth_panel.graph.selected = None;
        }
        changed = true;
    }
    if let Some(e) = pending_connect {
        graph.sub_mut(scope).connect(e);
        changed = true;
    }
    if let Some((id, pi, v)) = param_commit
        && let Some(n) = graph.sub_mut(scope).node_mut(id)
    {
        if n.params.len() <= pi {
            n.params.resize(n.kind.params().len(), 0.0);
        }
        n.params[pi] = v;
        changed = true;
    }
    if let Some((id, s)) = expr_commit
        && let Some(n) = graph.sub_mut(scope).node_mut(id)
    {
        n.expr = s;
        changed = true;
    }
    if changed {
        try_commit(app, instance_id, patch.clone(), graph);
    }
}
