//! Node-based materials: a small graph of texture nodes (noise, Voronoi
//! cells, patterns, images, mixing, maths and colour ramps) that computes a
//! surface's colour, roughness, metalness and height over one tile.
//!
//! Every source node repeats an integer number of times across the tile,
//! so the whole graph tiles seamlessly. That lets one baked tile stand for
//! the graph everywhere: the viewport, agent renders and glTF export all
//! use [`bake`], and the texture's projection (box, triplanar or UVs) and
//! `scale` place it on the surface like any other texture.

use std::collections::{BTreeMap, HashMap};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, Material, check_color};
use crate::image::{ImageAsset, Pixels};
use crate::texture::{self, Pattern};

/// Most nodes in one graph.
pub const MAX_NODES: usize = 64;
const MAX_STOPS: usize = 16;

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

/// A node input: a number, a `#rrggbb` colour, or the output of another
/// node (`{"node": "id"}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Input {
    Number(f64),
    Color(String),
    Link { node: String },
}

impl Input {
    fn link(&self) -> Option<&str> {
        match self {
            Input::Link { node } => Some(node),
            _ => None,
        }
    }
}

fn d_scale() -> Input {
    Input::Number(4.0)
}
fn d_one() -> Input {
    Input::Number(1.0)
}
fn d_half() -> Input {
    Input::Number(0.5)
}
fn d_zero() -> Input {
    Input::Number(0.0)
}
fn d_black() -> Input {
    Input::Color("#000000".into())
}
fn d_white() -> Input {
    Input::Color("#ffffff".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VoronoiOutput {
    /// Distance to the nearest cell centre (0 at the centre).
    #[default]
    Distance,
    /// A random value per cell (flat-coloured cells).
    Cells,
    /// 0 on the borders between cells, rising inside them.
    Edges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[default]
    U,
    V,
    /// From the tile's centre outward.
    Radial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MathOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
    Minimum,
    Maximum,
    /// 1 where a > b, else 0.
    GreaterThan,
    /// 1 where a < b, else 0.
    LessThan,
    /// |a|
    Absolute,
    /// A soft threshold of `a` at 0.5, `b` wide on either side.
    Smoothstep,
}

/// One colour-ramp stop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Stop {
    /// Position along the ramp, 0-1.
    pub at: f64,
    /// `#rrggbb`
    pub color: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    /// Fractal value noise, 0-1. `scale` is how many cells repeat across
    /// the tile (rounded, so it tiles); `detail` the number of octaves.
    Noise {
        #[serde(default = "d_scale")]
        scale: Input,
        #[serde(default = "d_scale")]
        detail: Input,
        /// Shift the lookup by this value (domain warping).
        #[serde(default = "d_zero")]
        warp: Input,
    },
    /// Voronoi cells: `scale` cells across the tile.
    Voronoi {
        #[serde(default = "d_scale")]
        scale: Input,
        #[serde(default)]
        output: VoronoiOutput,
        #[serde(default = "d_zero")]
        warp: Input,
    },
    /// One of the procedural patterns (how much of its second colour shows),
    /// repeated `scale` times across the tile.
    Pattern {
        pattern: Pattern,
        #[serde(default = "d_one")]
        scale: Input,
        #[serde(default = "d_zero")]
        warp: Input,
    },
    /// A ramp from 0 to 1 across the tile.
    Gradient {
        #[serde(default)]
        direction: Direction,
    },
    /// A scene image, repeated `scale` times across the tile.
    Image {
        image: String,
        #[serde(default = "d_one")]
        scale: Input,
    },
    /// Blend `a` toward `b` by `factor`.
    Mix {
        #[serde(default = "d_black")]
        a: Input,
        #[serde(default = "d_white")]
        b: Input,
        #[serde(default = "d_half")]
        factor: Input,
    },
    /// Arithmetic on two values (colours count as their brightness).
    Math {
        op: MathOp,
        #[serde(default = "d_zero")]
        a: Input,
        #[serde(default = "d_zero")]
        b: Input,
    },
    /// Map a 0-1 value to colours through stops (sorted by `at`).
    Ramp {
        #[serde(default = "d_half")]
        factor: Input,
        stops: Vec<Stop>,
        /// Hard steps instead of smooth blends.
        #[serde(default)]
        constant: bool,
    },
}

/// A node with a unique `id`; `at` is its place in the node editor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    pub id: String,
    #[serde(flatten)]
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
}

/// What the graph drives. Unset outputs keep the material's own values.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Outputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Input>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<Input>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metalness: Option<Input>,
    /// Height for relief (0 low, 1 high); the texture's `relief` sets depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<Input>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Graph {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub output: Outputs,
}

impl Graph {
    fn inputs(&self) -> impl Iterator<Item = (&str, &Input)> {
        self.nodes.iter().flat_map(|n| {
            let id = n.id.as_str();
            let list: Vec<&Input> = match &n.kind {
                NodeKind::Noise {
                    scale,
                    detail,
                    warp,
                } => vec![scale, detail, warp],
                NodeKind::Voronoi { scale, warp, .. } | NodeKind::Pattern { scale, warp, .. } => {
                    vec![scale, warp]
                }
                NodeKind::Gradient { .. } => vec![],
                NodeKind::Image { scale, .. } => vec![scale],
                NodeKind::Mix { a, b, factor } => vec![a, b, factor],
                NodeKind::Math { a, b, .. } => vec![a, b],
                NodeKind::Ramp { factor, .. } => vec![factor],
            };
            list.into_iter().map(move |i| (id, i))
        })
    }

    fn outputs(&self) -> impl Iterator<Item = &Input> {
        let o = &self.output;
        [&o.color, &o.roughness, &o.metalness, &o.height]
            .into_iter()
            .flatten()
    }

    /// Scene images the graph samples.
    pub fn images(&self) -> impl Iterator<Item = &str> {
        self.nodes.iter().filter_map(|n| match &n.kind {
            NodeKind::Image { image, .. } => Some(image.as_str()),
            _ => None,
        })
    }

    /// Check ids, links, colours and ranges, and that there are no cycles.
    pub fn validate(&self) -> Result<(), EngineError> {
        self.order().map(|_| ())
    }

    /// Node indices so every node comes after the nodes it reads.
    fn order(&self) -> Result<Vec<usize>, EngineError> {
        if self.nodes.len() > MAX_NODES {
            return err(format!("a node graph holds at most {MAX_NODES} nodes"));
        }
        let mut index = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.id.is_empty() || n.id.len() > 40 {
                return err("node ids must be 1-40 characters");
            }
            if index.insert(n.id.as_str(), i).is_some() {
                return err(format!("duplicate node id {:?}", n.id));
            }
            if let NodeKind::Ramp { stops, .. } = &n.kind {
                if stops.is_empty() || stops.len() > MAX_STOPS {
                    return err(format!("a ramp needs 1-{MAX_STOPS} stops"));
                }
                for s in stops {
                    check_color(&s.color)?;
                    if !(0.0..=1.0).contains(&s.at) {
                        return err("ramp stops sit between 0 and 1");
                    }
                }
            }
        }
        for input in self.inputs().map(|(_, i)| i).chain(self.outputs()) {
            match input {
                Input::Number(x) if !x.is_finite() => return err("node numbers must be finite"),
                Input::Color(c) => {
                    check_color(c)?;
                }
                Input::Link { node } if !index.contains_key(node.as_str()) => {
                    return err(format!("no node with id {node:?}"));
                }
                _ => {}
            }
        }
        // Depth-first topological order; a node met again while on the
        // stack closes a cycle.
        let mut deps: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        for (id, input) in self.inputs() {
            if let Some(l) = input.link() {
                deps[index[id]].push(index[l]);
            }
        }
        let mut state = vec![0u8; self.nodes.len()];
        let mut order = Vec::with_capacity(self.nodes.len());
        fn visit(
            i: usize,
            deps: &[Vec<usize>],
            state: &mut [u8],
            order: &mut Vec<usize>,
            nodes: &[Node],
        ) -> Result<(), EngineError> {
            match state[i] {
                1 => return err(format!("node {:?} is part of a loop", nodes[i].id)),
                2 => return Ok(()),
                _ => {}
            }
            state[i] = 1;
            for &d in &deps[i] {
                visit(d, deps, state, order, nodes)?;
            }
            state[i] = 2;
            order.push(i);
            Ok(())
        }
        for i in 0..self.nodes.len() {
            visit(i, &deps, &mut state, &mut order, &self.nodes)?;
        }
        Ok(order)
    }
}

#[derive(Debug, Clone, Copy)]
enum Val {
    Scalar(f64),
    /// sRGB, 0-1.
    Color([f64; 3]),
}

impl Val {
    fn scalar(self) -> f64 {
        match self {
            Val::Scalar(x) => x,
            Val::Color(c) => 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2],
        }
    }

    fn color(self) -> [f64; 3] {
        match self {
            Val::Scalar(x) => [x; 3],
            Val::Color(c) => c,
        }
    }
}

fn hex(c: &str) -> [f64; 3] {
    let ch = |i: usize| u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    [ch(1), ch(3), ch(5)]
}

/// Integer repeat count from a scale input.
fn repeats(x: f64, max: f64) -> i64 {
    x.round().clamp(1.0, max) as i64
}

fn hash2(x: i64, y: i64, seed: i64) -> (f64, f64) {
    (texture::hash(x, y, seed), texture::hash(x, y, seed + 7919))
}

/// Tileable Voronoi with `n` cells across: (nearest distance, second
/// distance, nearest cell's random value), distances in cell units.
fn voronoi(u: f64, v: f64, n: i64) -> (f64, f64, f64) {
    let (x, y) = (u.rem_euclid(1.0) * n as f64, v.rem_euclid(1.0) * n as f64);
    let (cx, cy) = (x.floor() as i64, y.floor() as i64);
    let (mut d1, mut d2, mut id) = (f64::INFINITY, f64::INFINITY, 0.0);
    for j in -1..=1 {
        for i in -1..=1 {
            let (gx, gy) = (cx + i, cy + j);
            let (wx, wy) = (gx.rem_euclid(n), gy.rem_euclid(n));
            let (jx, jy) = hash2(wx, wy, 31);
            let (px, py) = (gx as f64 + 0.1 + 0.8 * jx, gy as f64 + 0.1 + 0.8 * jy);
            let d = ((px - x).powi(2) + (py - y).powi(2)).sqrt();
            if d < d1 {
                d2 = d1;
                d1 = d;
                id = texture::hash(wx, wy, 53);
            } else if d < d2 {
                d2 = d;
            }
        }
    }
    (d1, d2, id)
}

fn ramp(stops: &[Stop], t: f64, constant: bool) -> [f64; 3] {
    let mut sorted: Vec<(f64, [f64; 3])> = stops.iter().map(|s| (s.at, hex(&s.color))).collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let t = t.clamp(0.0, 1.0);
    if t <= sorted[0].0 {
        return sorted[0].1;
    }
    for w in sorted.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if t <= b {
            if constant {
                return ca;
            }
            let k = if b > a { (t - a) / (b - a) } else { 1.0 };
            return [0, 1, 2].map(|i| ca[i] + (cb[i] - ca[i]) * k);
        }
    }
    sorted[sorted.len() - 1].1
}

/// A graph ready to evaluate: nodes in dependency order, images decoded.
pub struct Compiled<'a> {
    graph: &'a Graph,
    order: Vec<usize>,
    index: HashMap<&'a str, usize>,
    images: HashMap<&'a str, &'a Pixels>,
}

/// Per-point results; `None` where the output is not connected.
pub struct Sample {
    pub color: Option<[f64; 3]>,
    pub roughness: Option<f64>,
    pub metalness: Option<f64>,
    pub height: Option<f64>,
}

impl<'a> Compiled<'a> {
    pub fn new(
        graph: &'a Graph,
        images: HashMap<&'a str, &'a Pixels>,
    ) -> Result<Self, EngineError> {
        let order = graph.order()?;
        let index = graph
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        Ok(Compiled {
            graph,
            order,
            index,
            images,
        })
    }

    /// Evaluate at (u, v) in tile units.
    pub fn sample(&self, u: f64, v: f64) -> Sample {
        let mut vals = vec![Val::Scalar(0.0); self.graph.nodes.len()];
        let get = |vals: &[Val], input: &Input| -> Val {
            match input {
                Input::Number(x) => Val::Scalar(*x),
                Input::Color(c) => Val::Color(hex(c)),
                Input::Link { node } => vals[self.index[node.as_str()]],
            }
        };
        for &i in &self.order {
            let s = |input: &Input| get(&vals, input).scalar();
            vals[i] = match &self.graph.nodes[i].kind {
                NodeKind::Noise {
                    scale,
                    detail,
                    warp,
                } => {
                    let (n, oct) = (repeats(s(scale), 64.0), repeats(s(detail), 8.0));
                    let w = s(warp);
                    let (mut sum, mut amp, mut cells, mut norm) = (0.0, 0.5, n, 0.0);
                    for k in 0..oct {
                        sum += texture::noise(u + w, v + w, cells, 101 + k) * amp;
                        norm += amp;
                        amp *= 0.5;
                        cells *= 2;
                    }
                    Val::Scalar(sum / norm)
                }
                NodeKind::Voronoi {
                    scale,
                    output,
                    warp,
                } => {
                    let w = s(warp);
                    let (d1, d2, id) = voronoi(u + w, v + w, repeats(s(scale), 64.0));
                    Val::Scalar(match output {
                        VoronoiOutput::Distance => (d1 / 0.9).min(1.0),
                        VoronoiOutput::Cells => id,
                        VoronoiOutput::Edges => ((d2 - d1) * 2.0).min(1.0),
                    })
                }
                NodeKind::Pattern {
                    pattern,
                    scale,
                    warp,
                } => {
                    let (n, w) = (repeats(s(scale), 64.0) as f64, s(warp));
                    Val::Scalar(texture::sample(*pattern, (u + w) * n, (v + w) * n))
                }
                NodeKind::Gradient { direction } => Val::Scalar(match direction {
                    Direction::U => u.rem_euclid(1.0),
                    Direction::V => v.rem_euclid(1.0),
                    Direction::Radial => {
                        let (x, y) = (u.rem_euclid(1.0) - 0.5, v.rem_euclid(1.0) - 0.5);
                        ((x * x + y * y).sqrt() * 2.0).min(1.0)
                    }
                }),
                NodeKind::Image { image, scale } => match self.images.get(image.as_str()) {
                    Some(px) => {
                        let n = repeats(s(scale), 64.0) as f64;
                        Val::Color(px.sample(u * n, v * n))
                    }
                    None => Val::Color([1.0, 0.0, 1.0]),
                },
                NodeKind::Mix { a, b, factor } => {
                    let (a, b, f) = (get(&vals, a), get(&vals, b), s(factor).clamp(0.0, 1.0));
                    match (a, b) {
                        (Val::Scalar(x), Val::Scalar(y)) => Val::Scalar(x + (y - x) * f),
                        _ => {
                            let (x, y) = (a.color(), b.color());
                            Val::Color([0, 1, 2].map(|k| x[k] + (y[k] - x[k]) * f))
                        }
                    }
                }
                NodeKind::Math { op, a, b } => {
                    let (a, b) = (s(a), s(b));
                    Val::Scalar(match op {
                        MathOp::Add => a + b,
                        MathOp::Subtract => a - b,
                        MathOp::Multiply => a * b,
                        MathOp::Divide => {
                            if b.abs() < 1e-12 {
                                0.0
                            } else {
                                a / b
                            }
                        }
                        MathOp::Power => {
                            let p = a.abs().powf(b);
                            if p.is_finite() { p } else { 0.0 }
                        }
                        MathOp::Minimum => a.min(b),
                        MathOp::Maximum => a.max(b),
                        MathOp::GreaterThan => f64::from(u8::from(a > b)),
                        MathOp::LessThan => f64::from(u8::from(a < b)),
                        MathOp::Absolute => a.abs(),
                        MathOp::Smoothstep => {
                            let w = b.abs().max(1e-6);
                            let t = ((a - (0.5 - w)) / (2.0 * w)).clamp(0.0, 1.0);
                            t * t * (3.0 - 2.0 * t)
                        }
                    })
                }
                NodeKind::Ramp {
                    factor,
                    stops,
                    constant,
                } => Val::Color(ramp(stops, s(factor), *constant)),
            };
        }
        let o = &self.graph.output;
        let out = |input: &Option<Input>| input.as_ref().map(|i| get(&vals, i));
        Sample {
            color: out(&o.color).map(|v| v.color().map(|c| c.clamp(0.0, 1.0))),
            roughness: out(&o.roughness).map(|v| v.scalar().clamp(0.0, 1.0)),
            metalness: out(&o.metalness).map(|v| v.scalar().clamp(0.0, 1.0)),
            height: out(&o.height).map(|v| v.scalar().clamp(0.0, 1.0)),
        }
    }
}

/// A graph baked into tiles (row 0 at the top, v up like our UVs).
pub struct Baked {
    /// sRGB albedo.
    pub color: Pixels,
    /// Height in every channel (0-1), when the height output is connected.
    pub height: Option<Pixels>,
    /// glTF-style occlusion/roughness/metalness (R unused = 1, G roughness,
    /// B metalness), when either output is connected.
    pub orm: Option<Pixels>,
}

/// Bake the graph over one tile, `size` pixels square. `base` colour,
/// `roughness` and `metalness` fill in for unconnected outputs.
pub fn bake(
    graph: &Graph,
    images: HashMap<&str, &Pixels>,
    base: &str,
    roughness: f64,
    metalness: f64,
    size: u32,
) -> Result<Baked, EngineError> {
    let compiled = Compiled::new(graph, images)?;
    let o = &graph.output;
    let n = (size * size) as usize;
    let (mut color, mut height, mut orm) = (Vec::with_capacity(n), Vec::new(), Vec::new());
    let base = hex(base);
    for y in 0..size {
        for x in 0..size {
            let (u, v) = (
                (x as f64 + 0.5) / size as f64,
                1.0 - (y as f64 + 0.5) / size as f64,
            );
            let s = compiled.sample(u, v);
            color.push(s.color.unwrap_or(base).map(|c| c as f32));
            if o.height.is_some() {
                height.push([s.height.unwrap_or(0.0) as f32; 3]);
            }
            if o.roughness.is_some() || o.metalness.is_some() {
                orm.push([
                    1.0,
                    s.roughness.unwrap_or(roughness) as f32,
                    s.metalness.unwrap_or(metalness) as f32,
                ]);
            }
        }
    }
    let px = |rgb: Vec<[f32; 3]>| Pixels {
        width: size,
        height: size,
        rgb,
    };
    Ok(Baked {
        color: px(color),
        height: (!height.is_empty()).then(|| px(height)),
        orm: (!orm.is_empty()).then(|| px(orm)),
    })
}

/// Bake the node graph of a material's texture, decoding the scene images
/// it samples; `None` when the material has no graph.
pub fn bake_material(
    material: &Material,
    images: &BTreeMap<String, ImageAsset>,
    size: u32,
) -> Option<Baked> {
    let graph = material.texture.as_ref()?.graph.as_ref()?;
    let decoded: Vec<(&str, Pixels)> = graph
        .images()
        .filter_map(|name| Some((name, images.get(name)?.decode().ok()?)))
        .collect();
    let refs = decoded.iter().map(|(n, p)| (*n, p)).collect();
    bake(
        graph,
        refs,
        &material.color,
        material.roughness,
        material.metalness,
        size,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn graph(v: serde_json::Value) -> Graph {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn evaluates_links_ramps_and_maths() {
        let g = graph(json!({
            "nodes": [
                {"id": "n", "type": "noise", "scale": 3, "detail": 2},
                {"id": "r", "type": "ramp", "factor": {"node": "n"}, "stops": [{"at": 0, "color": "#000000"}, {"at": 1, "color": "#ffffff"}]},
                {"id": "edge", "type": "math", "op": "greater_than", "a": {"node": "n"}, "b": 0.5}
            ],
            "output": {"color": {"node": "r"}, "roughness": {"node": "edge"}, "metalness": 0.25}
        }));
        let c = Compiled::new(&g, HashMap::new()).unwrap();
        for k in 0..40 {
            let (u, v) = (k as f64 * 0.137, k as f64 * 0.071);
            let s = c.sample(u, v);
            let col = s.color.unwrap();
            assert!((col[0] - col[1]).abs() < 1e-12, "a grey ramp");
            let rough = s.roughness.unwrap();
            assert!(rough == 0.0 || rough == 1.0);
            assert!(
                (rough == 1.0) == (col[0] > 0.5),
                "threshold matches the ramp"
            );
            assert_eq!(s.metalness, Some(0.25));
            assert!(s.height.is_none());
            // The graph tiles: one tile over, the same value.
            let t = c.sample(u + 1.0, v - 2.0);
            assert!((t.color.unwrap()[0] - col[0]).abs() < 1e-9);
        }
    }

    #[test]
    fn voronoi_and_patterns_tile_seamlessly() {
        for output in ["distance", "cells", "edges"] {
            let g = graph(json!({
                "nodes": [{"id": "v", "type": "voronoi", "scale": 5, "output": output}],
                "output": {"height": {"node": "v"}}
            }));
            let c = Compiled::new(&g, HashMap::new()).unwrap();
            for k in 0..30 {
                let t = k as f64 / 30.0;
                let (a, b) = (
                    c.sample(1e-9, t).height.unwrap(),
                    c.sample(1.0 - 1e-9, t).height.unwrap(),
                );
                assert!(
                    (a - b).abs() < 1e-3 || output == "cells",
                    "{output} seam at {t}: {a} vs {b}"
                );
                assert!((0.0..=1.0).contains(&a));
            }
        }
        let g = graph(json!({
            "nodes": [{"id": "p", "type": "pattern", "pattern": "checker", "scale": 2}],
            "output": {"color": {"node": "p"}}
        }));
        let baked = bake(&g, HashMap::new(), "#808080", 0.5, 0.0, 8).unwrap();
        assert_eq!(baked.color.rgb.len(), 64);
        assert!(baked.orm.is_none() && baked.height.is_none());
        // Two repeats of a 2x2 checker over 8 pixels: alternates every 2.
        assert_ne!(baked.color.rgb[0], baked.color.rgb[2]);
        assert_eq!(baked.color.rgb[0], baked.color.rgb[4]);
    }

    #[test]
    fn rejects_bad_graphs() {
        let bad = [
            json!({"nodes": [{"id": "a", "type": "math", "op": "add", "a": {"node": "b"}}, {"id": "b", "type": "math", "op": "add", "a": {"node": "a"}}]}),
            json!({"nodes": [{"id": "a", "type": "noise"}, {"id": "a", "type": "noise"}]}),
            json!({"nodes": [], "output": {"color": {"node": "ghost"}}}),
            json!({"nodes": [{"id": "r", "type": "ramp", "stops": []}]}),
            json!({"nodes": [{"id": "m", "type": "mix", "a": "red"}]}),
        ];
        for b in bad {
            assert!(graph(b.clone()).validate().is_err(), "{b}");
        }
        assert!(
            serde_json::from_value::<Graph>(json!({"nodes": [{"id": "x", "type": "teleport"}]}))
                .is_err()
        );
    }
}
