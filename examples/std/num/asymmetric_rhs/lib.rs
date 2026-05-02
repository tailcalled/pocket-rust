// Asymmetric `impl Mul<u32> for Vec3 { type Output = Vec3; }` style:
// the operator's RHS type doesn't equal Self. Validates the
// `<Rhs = Self>` default isn't forced — users can specify a
// non-Self Rhs and pick a custom Output type. (For the Vec3 case
// `Self == Output`, but the unifier should still distinguish.)

struct Vec3 { x: u32, y: u32, z: u32 }

impl Mul<u32> for Vec3 {
    type Output = Vec3;
    fn mul(self, k: u32) -> Vec3 {
        Vec3 { x: self.x * k, y: self.y * k, z: self.z * k }
    }
}

fn answer() -> u32 {
    let v = Vec3 { x: 6, y: 7, z: 8 };
    let s = v * 1u32;
    s.x * s.y
}
