struct Point { x: u32, y: u32 }

impl Point {
    fn new(x: u32, y: u32) -> Self {
        Self { x: x, y: y }
    }

    fn origin() -> Self {
        Self::new(0, 0)
    }

    fn x(&self) -> u32 {
        self.x
    }

    fn x_ref(&self) -> &u32 {
        &self.x
    }

    fn set_x(&mut self, v: u32) -> u32 {
        self.x = v;
        self.x
    }

    fn x_mut(&mut self) -> &mut u32 {
        &mut self.x
    }

    fn into_x(self) -> u32 {
        self.x
    }
}

fn answer() -> u32 {
    let mut pt = Point::new(7, 14);
    let _o = Point::origin();
    let _a = pt.x();
    let _b = { let r: &u32 = pt.x_ref(); *r };
    let _c = pt.set_x(20);
    let m: &mut u32 = pt.x_mut();
    *m = 42;
    let _d = Point::set_x(&mut pt, 42);
    let consumed = Point { x: 99, y: 0 };
    let _e = consumed.into_x();
    pt.x
}
