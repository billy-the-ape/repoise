$ErrorActionPreference = "Stop"
Push-Location (Join-Path $PSScriptRoot "..")
try {
    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw "Formatting failed" }
    cargo clippy --workspace --all-targets --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "Clippy failed" }
    cargo test --workspace --locked
    if ($LASTEXITCODE -ne 0) { throw "Tests failed" }
    $previous = $env:RUSTDOCFLAGS
    try {
        $env:RUSTDOCFLAGS = "-D warnings"
        cargo doc --workspace --no-deps --locked
        if ($LASTEXITCODE -ne 0) { throw "Documentation build failed" }
    } finally { $env:RUSTDOCFLAGS = $previous }
} finally { Pop-Location }
