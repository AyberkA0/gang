//! Local transforms (position, rotation, scale) of Unity `Transform`
//! documents, for moving an object to a new parent without moving it in the
//! world.

use unity_yaml::{Map, Scalar, Value};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trs {
    pub position: [f64; 3],
    /// Quaternion x, y, z, w.
    pub rotation: [f64; 4],
    pub scale: [f64; 3],
}

impl Trs {
    pub const IDENTITY: Trs = Trs {
        position: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
    };

    /// The local transform of a `Transform` body; missing fields are identity.
    pub fn read(body: &Map) -> Trs {
        let vector = |key: &str, names: &[&str], default: f64| -> Vec<f64> {
            let scalar = body.get(key).and_then(Value::as_scalar);
            names
                .iter()
                .map(|n| {
                    scalar
                        .and_then(|s| s.flow_get(n))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(default)
                })
                .collect()
        };
        let p = vector("m_LocalPosition", &["x", "y", "z"], 0.0);
        let s = vector("m_LocalScale", &["x", "y", "z"], 1.0);
        let r = body
            .get("m_LocalRotation")
            .and_then(Value::as_scalar)
            .map(|_| vector("m_LocalRotation", &["x", "y", "z", "w"], 0.0))
            .unwrap_or_else(|| vec![0.0, 0.0, 0.0, 1.0]);
        Trs {
            position: [p[0], p[1], p[2]],
            rotation: normalize([r[0], r[1], r[2], r[3]]),
            scale: [s[0], s[1], s[2]],
        }
    }

    /// `child` expressed in this transform's parent space. Shear from
    /// non-uniform scale under rotation is not representable and is dropped,
    /// as Unity does when re-parenting.
    pub fn then(&self, child: &Trs) -> Trs {
        let scaled = [
            child.position[0] * self.scale[0],
            child.position[1] * self.scale[1],
            child.position[2] * self.scale[2],
        ];
        let moved = rotate(self.rotation, scaled);
        Trs {
            position: [
                self.position[0] + moved[0],
                self.position[1] + moved[1],
                self.position[2] + moved[2],
            ],
            rotation: normalize(multiply(self.rotation, child.rotation)),
            scale: [
                self.scale[0] * child.scale[0],
                self.scale[1] * child.scale[1],
                self.scale[2] * child.scale[2],
            ],
        }
    }

    /// Writes position, rotation, scale and the inspector's Euler hint.
    pub fn write(&self, body: &mut Map) {
        let flow = |names: &[&str], values: &[f64]| {
            let entries: Vec<(String, String)> = names
                .iter()
                .zip(values)
                .map(|(n, v)| (n.to_string(), number(*v)))
                .collect();
            Value::Scalar(Scalar::flow(&entries))
        };
        body.set(
            "m_LocalRotation",
            flow(&["x", "y", "z", "w"], &self.rotation),
        );
        body.set("m_LocalPosition", flow(&["x", "y", "z"], &self.position));
        body.set("m_LocalScale", flow(&["x", "y", "z"], &self.scale));
        if body.get("m_LocalEulerAnglesHint").is_some() {
            body.set(
                "m_LocalEulerAnglesHint",
                flow(&["x", "y", "z"], &euler(self.rotation)),
            );
        }
    }
}

fn normalize(q: [f64; 4]) -> [f64; 4] {
    let length = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    if length < 1e-12 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    q.map(|v| v / length)
}

fn multiply(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let p = multiply(
        multiply(q, [v[0], v[1], v[2], 0.0]),
        [-q[0], -q[1], -q[2], q[3]],
    );
    [p[0], p[1], p[2]]
}

/// Unity's Euler angles (degrees, applied Z, then X, then Y).
fn euler(q: [f64; 4]) -> [f64; 3] {
    let [x, y, z, w] = q;
    let sin_x = (2.0 * (w * x - y * z)).clamp(-1.0, 1.0);
    let ex = sin_x.asin();
    let (ey, ez) = if sin_x.abs() > 0.9999 {
        (
            (2.0 * (w * y - x * z)).atan2(1.0 - 2.0 * (y * y + z * z)),
            0.0,
        )
    } else {
        (
            (2.0 * (w * y + x * z)).atan2(1.0 - 2.0 * (x * x + y * y)),
            (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (x * x + z * z)),
        )
    };
    [ex, ey, ez].map(|a| {
        let degrees = a.to_degrees();
        if degrees < -1e-4 {
            degrees + 360.0
        } else {
            degrees
        }
    })
}

/// Unity-style number text: the shortest `f32` that reads back the same.
fn number(value: f64) -> String {
    let value = if value.abs() < 1e-6 { 0.0 } else { value };
    format!("{}", value as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &[f64], b: &[f64]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    #[test]
    fn composing_keeps_the_world_position() {
        // A parent at (10, 0, 0), turned 90° around Y, scaled 2.
        let half = std::f64::consts::FRAC_1_SQRT_2;
        let parent = Trs {
            position: [10.0, 0.0, 0.0],
            rotation: [0.0, half, 0.0, half],
            scale: [2.0, 2.0, 2.0],
        };
        let child = Trs {
            position: [0.0, 0.0, 1.0],
            ..Trs::IDENTITY
        };
        let world = parent.then(&child);
        assert!(close(&world.position, &[12.0, 0.0, 0.0]), "{world:?}");
        assert!(close(&world.scale, &[2.0, 2.0, 2.0]));
        assert!(
            close(&euler(world.rotation), &[0.0, 90.0, 0.0]),
            "{:?}",
            euler(world.rotation)
        );
    }

    #[test]
    fn numbers_look_like_unity() {
        assert_eq!(number(4.8), "4.8");
        assert_eq!(number(1e-9), "0");
        assert_eq!(number(-std::f64::consts::FRAC_1_SQRT_2), "-0.70710677");
    }
}
