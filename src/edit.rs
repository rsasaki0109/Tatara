//! Component-level mesh edits: move vertices, bevel edges and loop cuts.
//! Every operation keeps a closed mesh closed and outward-facing.

use std::collections::{HashMap, HashSet};

use crate::engine::{EngineError, MAX_FACES, Mesh, Vec3};

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

fn key(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: Vec3, s: f64) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn length(a: Vec3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// Offset `a` by `w` toward `p`.
fn toward(a: Vec3, p: Vec3, w: f64) -> Vec3 {
    let d = sub(p, a);
    add(a, scale(d, w / length(d).max(1e-12)))
}

fn edge_set(mesh: &Mesh) -> HashSet<(u32, u32)> {
    let mut out = HashSet::new();
    for f in &mesh.faces {
        for k in 0..f.len() {
            out.insert(key(f[k], f[(k + 1) % f.len()]));
        }
    }
    out
}

pub fn move_vertices(mesh: &mut Mesh, vertices: &[u32], offset: Vec3) -> Result<(), EngineError> {
    if vertices.is_empty() {
        return err("select at least one vertex");
    }
    if offset.iter().any(|v| !v.is_finite() || v.abs() > 1e4) {
        return err("offset must contain finite numbers");
    }
    let unique: HashSet<u32> = vertices.iter().copied().collect();
    for &i in &unique {
        let Some(v) = mesh.vertices.get_mut(i as usize) else {
            return err(format!("vertex {i} does not exist"));
        };
        *v = add(*v, offset);
    }
    Ok(())
}

/// Bevel (chamfer) the given edges, or every edge when `edges` is `None`.
///
/// Each face corner touching a bevelled edge is replaced by points offset
/// `width` along the face's edges; bevelled edges become quads, and any hole
/// left at a vertex is closed with a polygon. A corner where two bevelled
/// edges meet moves along both, which gives the classic chamfered box.
pub fn bevel(mesh: &mut Mesh, edges: Option<&[[u32; 2]]>, width: f64) -> Result<(), EngineError> {
    if !(width.is_finite() && width > 0.0) {
        return err("width must be positive");
    }
    let all = edge_set(mesh);
    let bevelled: HashSet<(u32, u32)> = match edges {
        None => all.clone(),
        Some(list) => {
            if list.is_empty() {
                return err("select at least one edge");
            }
            let mut s = HashSet::new();
            for &[a, b] in list {
                if !all.contains(&key(a, b)) {
                    return err(format!("edge {a}-{b} does not exist"));
                }
                s.insert(key(a, b));
            }
            s
        }
    };
    // The bevel width must leave every touched edge with positive length.
    for f in &mesh.faces {
        for k in 0..f.len() {
            let (a, b) = (f[k], f[(k + 1) % f.len()]);
            let touches = bevelled
                .iter()
                .any(|&(x, y)| x == a || y == a || x == b || y == b);
            if touches {
                let len = length(sub(mesh.vertices[a as usize], mesh.vertices[b as usize]));
                if 2.0 * width >= len {
                    return err(format!(
                        "width {width} is too large for edge {a}-{b} (length {len:.3})"
                    ));
                }
            }
        }
    }
    let mut touched: HashSet<u32> = HashSet::new();
    for &(a, b) in &bevelled {
        touched.insert(a);
        touched.insert(b);
    }
    // Who owns directed edge a->b, to look across an edge.
    let mut owner: HashMap<(u32, u32), usize> = HashMap::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        for k in 0..f.len() {
            owner.insert((f[k], f[(k + 1) % f.len()]), fi);
        }
    }
    let prev_next = |fi: usize, a: u32| -> (u32, u32) {
        let f = &mesh.faces[fi];
        let k = f.iter().position(|&x| x == a).expect("corner in face");
        (f[(k + f.len() - 1) % f.len()], f[(k + 1) % f.len()])
    };
    let is_b = |a: u32, b: u32| bevelled.contains(&key(a, b));
    // Does face `g` (across an edge (a, p) from us) slide its corner `a` along it?
    let slides_along = |g: Option<&usize>, a: u32, p: u32| -> bool {
        let Some(&g) = g else {
            return false;
        };
        let (gp, gn) = prev_next(g, a);
        let other = if gp == p { gn } else { gp };
        is_b(a, other)
    };

    #[derive(Hash, PartialEq, Eq, Clone, Copy)]
    enum Point {
        Orig(u32),
        Along(u32, u32),
        Corner(usize, u32),
    }
    let mut ids: HashMap<Point, u32> = HashMap::new();
    let mut vertices: Vec<Vec3> = Vec::new();
    let mut origin: Vec<u32> = Vec::new();
    let v = &mesh.vertices;
    let mut point = |p: Point| -> u32 {
        *ids.entry(p).or_insert_with(|| {
            let (pos, from) = match p {
                Point::Orig(a) => (v[a as usize], a),
                Point::Along(a, q) => (toward(v[a as usize], v[q as usize], width), a),
                Point::Corner(_, a) => (v[a as usize], a), // placed below
            };
            vertices.push(pos);
            origin.push(from);
            (vertices.len() - 1) as u32
        })
    };

    let mut faces: Vec<Vec<u32>> = Vec::with_capacity(mesh.faces.len() * 2);
    // Per bevelled directed edge, the new endpoints this face uses.
    let mut ends: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    let mut corner_pos: Vec<(u32, Vec3)> = Vec::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let n = f.len();
        let mut lp: Vec<Vec<u32>> = Vec::with_capacity(n);
        for k in 0..n {
            let (p, a, nx) = (f[(k + n - 1) % n], f[k], f[(k + 1) % n]);
            let pts = if !touched.contains(&a) {
                vec![point(Point::Orig(a))]
            } else {
                match (is_b(p, a), is_b(a, nx)) {
                    (true, true) => {
                        let id = point(Point::Corner(fi, a));
                        let pos = add(
                            toward(v[a as usize], v[p as usize], width),
                            sub(toward(v[a as usize], v[nx as usize], width), v[a as usize]),
                        );
                        corner_pos.push((id, pos));
                        vec![id]
                    }
                    (false, true) => vec![point(Point::Along(a, p))],
                    (true, false) => vec![point(Point::Along(a, nx))],
                    (false, false) => {
                        let mut pts = Vec::new();
                        // Our edge p->a is a->p in the face across it, and a->nx is nx->a.
                        if slides_along(owner.get(&(a, p)), a, p) {
                            pts.push(point(Point::Along(a, p)));
                        }
                        if slides_along(owner.get(&(nx, a)), a, nx) {
                            pts.push(point(Point::Along(a, nx)));
                        }
                        if pts.is_empty() {
                            pts.push(point(Point::Orig(a)));
                        }
                        pts
                    }
                }
            };
            lp.push(pts);
        }
        for k in 0..n {
            let (a, b) = (f[k], f[(k + 1) % n]);
            if is_b(a, b) {
                ends.insert((a, b), (*lp[k].last().unwrap(), lp[(k + 1) % n][0]));
            }
        }
        faces.push(lp.into_iter().flatten().collect());
    }
    for (id, pos) in corner_pos {
        vertices[id as usize] = pos;
    }
    // One quad per bevelled edge, joining the two faces' new edges.
    let mut bevel_edges: Vec<(u32, u32)> = ends.keys().copied().filter(|&(a, b)| a < b).collect();
    bevel_edges.sort_unstable();
    for (a, b) in bevel_edges {
        if let (Some(&(fa, fb)), Some(&(gb, ga))) = (ends.get(&(a, b)), ends.get(&(b, a))) {
            faces.push(vec![fb, fa, ga, gb]);
        }
    }
    // Close the holes left at vertices: boundary loops made only of
    // copies of one original vertex.
    let mut directed: HashSet<(u32, u32)> = HashSet::new();
    for f in &faces {
        for k in 0..f.len() {
            directed.insert((f[k], f[(k + 1) % f.len()]));
        }
    }
    let mut next: HashMap<u32, u32> = HashMap::new();
    for &(a, b) in &directed {
        if !directed.contains(&(b, a)) && origin[a as usize] == origin[b as usize] {
            next.insert(a, b);
        }
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let starts: Vec<u32> = {
        let mut s: Vec<u32> = next.keys().copied().collect();
        s.sort_unstable();
        s
    };
    for start in starts {
        if seen.contains(&start) {
            continue;
        }
        let mut lp = vec![start];
        seen.insert(start);
        let mut cur = start;
        let closed = loop {
            match next.get(&cur) {
                Some(&n) if n == start => break true,
                Some(&n) if !seen.contains(&n) => {
                    seen.insert(n);
                    lp.push(n);
                    cur = n;
                }
                _ => break false,
            }
        };
        if closed && lp.len() >= 3 {
            lp.reverse();
            faces.push(lp);
        }
    }
    if faces.len() > MAX_FACES {
        return err(format!("bevel would exceed {MAX_FACES} faces"));
    }
    *mesh = Mesh { vertices, faces };
    drop_unused(mesh);
    Ok(())
}

/// Remove vertices no face references, renumbering faces.
fn drop_unused(mesh: &mut Mesh) {
    let mut used = vec![false; mesh.vertices.len()];
    for f in &mesh.faces {
        for &i in f {
            used[i as usize] = true;
        }
    }
    let mut remap = vec![0u32; mesh.vertices.len()];
    let mut vertices = Vec::new();
    for (i, v) in mesh.vertices.iter().enumerate() {
        if used[i] {
            remap[i] = vertices.len() as u32;
            vertices.push(*v);
        }
    }
    for f in &mut mesh.faces {
        for i in f.iter_mut() {
            *i = remap[*i as usize];
        }
    }
    mesh.vertices = vertices;
}

/// Cut a loop through the ring of quads crossing `edge`, placing the new
/// vertices at `fraction` along each ring edge (measured from the side of
/// the first edge's first vertex). Returns the new vertex indices in order.
pub fn loop_cut(mesh: &mut Mesh, edge: [u32; 2], fraction: f64) -> Result<Vec<u32>, EngineError> {
    if !(fraction.is_finite() && fraction > 0.0 && fraction < 1.0) {
        return err("fraction must be between 0 and 1 (exclusive)");
    }
    let mut faces_of: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        for k in 0..f.len() {
            faces_of
                .entry(key(f[k], f[(k + 1) % f.len()]))
                .or_default()
                .push(fi);
        }
    }
    let [a, b] = edge;
    if !faces_of.contains_key(&key(a, b)) {
        return err(format!("edge {a}-{b} does not exist"));
    }

    // Walk the ring in one direction from an oriented edge, collecting the
    // quads crossed and the oriented ring edges. Returns true for a closed loop.
    let walk = |start: (u32, u32),
                first_face: usize,
                ring: &mut Vec<(u32, u32)>,
                quads: &mut Vec<usize>|
     -> bool {
        let mut cur = start;
        let mut fi = first_face;
        loop {
            let f = &mesh.faces[fi];
            if f.len() != 4 || quads.contains(&fi) {
                return false;
            }
            let k = (0..4)
                .find(|&k| key(f[k], f[(k + 1) % 4]) == key(cur.0, cur.1))
                .expect("edge in face");
            // Keep the "same side" pairing: f[k] pairs with f[k+3], f[k+1] with f[k+2].
            let opposite = if f[k] == cur.0 {
                (f[(k + 3) % 4], f[(k + 2) % 4])
            } else {
                (f[(k + 2) % 4], f[(k + 3) % 4])
            };
            quads.push(fi);
            if key(opposite.0, opposite.1) == key(start.0, start.1) {
                return true;
            }
            ring.push(opposite);
            let across = faces_of[&key(opposite.0, opposite.1)]
                .iter()
                .copied()
                .find(|&g| g != fi);
            let Some(g) = across else { return false };
            fi = g;
            cur = opposite;
        }
    };

    let mut ring = vec![(a, b)];
    let mut quads = Vec::new();
    let first = faces_of[&key(a, b)].clone();
    let closed = walk((a, b), first[0], &mut ring, &mut quads);
    if !closed && first.len() > 1 {
        // Open strip: also walk the other way and prepend.
        let mut back_ring = Vec::new();
        let mut back_quads = quads.clone();
        walk((a, b), first[1], &mut back_ring, &mut back_quads);
        back_quads.drain(..quads.len());
        back_ring.reverse();
        back_ring.extend(ring);
        ring = back_ring;
        back_quads.extend(quads);
        quads = back_quads;
    }
    if quads.is_empty() {
        return err(format!("edge {a}-{b} is not part of a quad ring"));
    }
    if mesh.faces.len() + quads.len() > MAX_FACES {
        return err(format!("loop cut would exceed {MAX_FACES} faces"));
    }

    let mut mid: HashMap<(u32, u32), u32> = HashMap::new();
    let mut created = Vec::new();
    for &(s, e) in &ring {
        if mid.contains_key(&key(s, e)) {
            continue;
        }
        let p = add(
            mesh.vertices[s as usize],
            scale(
                sub(mesh.vertices[e as usize], mesh.vertices[s as usize]),
                fraction,
            ),
        );
        mesh.vertices.push(p);
        let id = (mesh.vertices.len() - 1) as u32;
        mid.insert(key(s, e), id);
        created.push(id);
    }
    let quad_set: HashSet<usize> = quads.iter().copied().collect();
    let mut out = Vec::with_capacity(mesh.faces.len() + quads.len());
    for (fi, f) in mesh.faces.iter().enumerate() {
        if quad_set.contains(&fi) {
            let k = (0..4)
                .find(|&k| {
                    mid.contains_key(&key(f[k], f[(k + 1) % 4]))
                        && mid.contains_key(&key(f[(k + 2) % 4], f[(k + 3) % 4]))
                })
                .expect("ring quad has two cut edges");
            let (q0, q1, q2, q3) = (f[k], f[(k + 1) % 4], f[(k + 2) % 4], f[(k + 3) % 4]);
            let m01 = mid[&key(q0, q1)];
            let m23 = mid[&key(q2, q3)];
            out.push(vec![q0, m01, m23, q3]);
            out.push(vec![m01, q1, q2, m23]);
        } else {
            // Faces beside the ends of an open ring get the new vertex too.
            let mut g = Vec::with_capacity(f.len() + 2);
            for k in 0..f.len() {
                let (x, y) = (f[k], f[(k + 1) % f.len()]);
                g.push(x);
                if let Some(&m) = mid.get(&key(x, y)) {
                    g.push(m);
                }
            }
            out.push(g);
        }
    }
    mesh.faces = out;
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Primitive, build_primitive, catmull_clark, extrude};

    fn cube(size: f64) -> Mesh {
        build_primitive(&Primitive::Cube { size }).unwrap()
    }

    fn volume(m: &Mesh) -> f64 {
        let mut vol = 0.0;
        for f in &m.faces {
            let a = m.vertices[f[0] as usize];
            for k in 1..f.len() - 1 {
                let b = m.vertices[f[k] as usize];
                let c = m.vertices[f[k + 1] as usize];
                vol += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0]);
            }
        }
        vol / 6.0
    }

    fn assert_closed(m: &Mesh) {
        let mut directed = HashMap::new();
        for f in &m.faces {
            assert!(f.len() >= 3, "degenerate face {f:?}");
            for k in 0..f.len() {
                *directed.entry((f[k], f[(k + 1) % f.len()])).or_insert(0) += 1;
            }
        }
        for (&(a, b), &n) in &directed {
            assert_eq!(n, 1, "edge {a}->{b} used {n} times");
            assert!(directed.contains_key(&(b, a)), "edge {a}->{b} has no twin");
        }
    }

    #[test]
    fn chamfer_every_edge_of_a_cube() {
        let mut m = cube(2.0);
        bevel(&mut m, None, 0.2).unwrap();
        assert_closed(&m);
        // 6 faces + 12 edge quads + 8 corner triangles.
        assert_eq!(m.faces.len(), 26);
        assert_eq!(m.vertices.len(), 24);
        // Removed: 12 edge prisms (legs 0.2, length 1.6) and 8 corners
        // (a 0.2 cube minus the tetrahedron that stays).
        let w: f64 = 0.2;
        let expected =
            8.0 - 12.0 * (w * w / 2.0) * (2.0 - 2.0 * w) - 8.0 * (w.powi(3) - w.powi(3) / 6.0);
        let v = volume(&m);
        assert!((v - expected).abs() < 1e-9, "volume {v} vs {expected}");
    }

    #[test]
    fn bevel_one_edge() {
        let mut m = cube(2.0);
        // Top face is [7, 6, 2, 3]; bevel its +X edge 6-2.
        bevel(&mut m, Some(&[[6, 2]]), 0.25).unwrap();
        assert_closed(&m);
        assert_eq!(m.faces.len(), 7);
        let v = volume(&m);
        let expected = 8.0 - 0.25 * 0.25 / 2.0 * 2.0;
        assert!((v - expected).abs() < 1e-9, "volume {v} vs {expected}");
    }

    #[test]
    fn bevel_two_edges_sharing_a_vertex() {
        let mut m = cube(2.0);
        bevel(&mut m, Some(&[[6, 2], [7, 6]]), 0.2).unwrap();
        assert_closed(&m);
        assert!(volume(&m) < 8.0);
        bevel(&mut cube(2.0), Some(&[[0, 6]]), 0.2).unwrap_err();
        bevel(&mut cube(2.0), None, 1.5).unwrap_err();
    }

    #[test]
    fn bevel_then_subdivide_stays_closed() {
        let mut m = cube(2.0);
        extrude(&mut m, 4, 1.0).unwrap();
        bevel(&mut m, None, 0.1).unwrap();
        assert_closed(&m);
        let s = catmull_clark(&m);
        assert_closed(&s);
        assert!(volume(&s) > 0.0);
    }

    #[test]
    fn loop_cut_around_a_cube() {
        let mut m = cube(2.0);
        let created = loop_cut(&mut m, [7, 6], 0.25).unwrap();
        // Ring through four side faces: four new vertices, four extra faces.
        assert_eq!(created.len(), 4);
        assert_eq!(m.faces.len(), 10);
        assert_closed(&m);
        assert!((volume(&m) - 8.0).abs() < 1e-9);
        // All new vertices sit on one plane, a quarter of the way from the first edge.
        let first = m.vertices[created[0] as usize];
        let axis = (0..3).find(|&k| {
            created
                .iter()
                .all(|&i| (m.vertices[i as usize][k] - first[k]).abs() < 1e-9)
        });
        assert!(axis.is_some(), "cut is planar");
    }

    #[test]
    fn loop_cut_on_an_open_strip() {
        let mut plane = build_primitive(&Primitive::Plane { size: 2.0 }).unwrap();
        let created = loop_cut(&mut plane, [0, 1], 0.5).unwrap();
        assert_eq!(created.len(), 2);
        assert_eq!(plane.faces.len(), 2);
    }

    #[test]
    fn loop_cut_with_triangle_neighbours_inserts_vertices() {
        // A cone has quad-free caps, so a ring stops; neighbouring faces gain the cut vertex.
        let mut m = cube(2.0);
        bevel(&mut m, None, 0.2).unwrap();
        let before = m.faces.len();
        let edge = [m.faces[0][0], m.faces[0][1]];
        loop_cut(&mut m, edge, 0.5).unwrap();
        assert!(m.faces.len() > before);
        assert_closed(&m);
    }

    #[test]
    fn move_vertices_validates() {
        let mut m = cube(1.0);
        move_vertices(&mut m, &[6, 7, 7], [0.0, 1.0, 0.0]).unwrap();
        assert!((m.vertices[7][1] - 1.5).abs() < 1e-12);
        assert!(move_vertices(&mut m, &[99], [0.0; 3]).is_err());
        assert!(move_vertices(&mut m, &[], [0.0; 3]).is_err());
    }
}
