struct Point { x: usize, y: usize }

fn answer() -> usize {
    let pt1 = Point { x: 5, y: 10 };
    let v = { let r = &pt1; r.x };
    let q = pt1;
    q.x
}
