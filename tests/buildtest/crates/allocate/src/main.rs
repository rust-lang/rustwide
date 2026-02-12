pub fn allocate() {}

fn main() {
    let mb = std::env::args()
        .nth(1)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(512);

    let max_allocate = mb * 1024 * 1024;
    let data = [0u8; 4096];
    let mut allocated = 0;

    while allocated < max_allocate {
        Box::leak(Box::new(data));
        allocated += data.len();
    }

    println!("Allocated {} bytes of memory!", max_allocate);
}
