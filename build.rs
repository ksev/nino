fn main() -> std::io::Result<()> {
    prost_build::compile_protos(&["net.proto"], &["./proto/"])?;

    Ok(())
}
