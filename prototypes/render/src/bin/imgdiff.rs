//! Compare two RGBA8 PNGs: number of differing pixels and the max channel delta.
//! Usage: imgdiff a.png b.png

fn load(path: &str) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(path).expect("open"),
    ));
    let mut reader = decoder.read_info().expect("png header");
    let mut buf = vec![0; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut buf).expect("png frame");
    assert_eq!(
        info.color_type,
        png::ColorType::Rgba,
        "{path}: expected RGBA"
    );
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (wa, ha, a) = load(&args[1]);
    let (wb, hb, b) = load(&args[2]);
    assert_eq!((wa, ha), (wb, hb), "size mismatch");
    let mut differing = 0usize;
    let mut over2 = 0usize;
    let mut max_delta = 0u8;
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0.iter()) {
        let d = pa
            .iter()
            .zip(pb.iter())
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap();
        if d > 0 {
            differing += 1;
        }
        if d > 2 {
            over2 += 1;
        }
        max_delta = max_delta.max(d);
    }
    let total = (wa * ha) as usize;
    println!(
        "{} vs {}: {wa}x{ha}, differing pixels {differing} ({:.3}%), >2 levels {over2} ({:.3}%), max channel delta {max_delta}",
        args[1],
        args[2],
        100.0 * differing as f64 / total as f64,
        100.0 * over2 as f64 / total as f64
    );
}
