$ErrorActionPreference = 'Stop'
$earthCraftDir = Join-Path $PSScriptRoot 'EarthCraft'
if (-not (Test-Path (Join-Path $earthCraftDir 'Cargo.toml'))) {
    $earthCraftDir = $PSScriptRoot
}
Set-Location $earthCraftDir
$toolsDir = Join-Path $PSScriptRoot '.build'
if (Test-Path "$toolsDir/cargo/bin/cargo.exe") {
    $env:CARGO_HOME = "$toolsDir/cargo"
    $env:RUSTUP_HOME = "$toolsDir/rustup"
    $llvm = Get-ChildItem $toolsDir -Directory -Filter 'llvm-mingw*' | Select-Object -First 1
    if (-not $llvm) { throw 'The bundled LLVM-MinGW toolchain is missing.' }
    # GNU ld in this MinGW distribution mishandles its own paths when installed
    # beneath a directory containing spaces, so keep the compiler beside the project.
    $mingw = Join-Path ([IO.Path]::GetPathRoot($PSScriptRoot)) 'EarthPulseBuildTools'
    if (-not (Test-Path (Join-Path $mingw 'bin/x86_64-w64-mingw32-gcc.exe'))) {
        throw "Install the MSVCRT MinGW toolchain at $mingw (the project path contains spaces)."
    }
    $env:PATH = "$mingw/bin;$toolsDir/cargo/bin;$($llvm.FullName)/bin;$PSScriptRoot/.venv/Scripts;$env:PATH"
    $env:CC = 'x86_64-w64-mingw32-gcc'
    $env:CXX = 'x86_64-w64-mingw32-g++'
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = 'x86_64-w64-mingw32-gcc'
    $env:CMAKE_GENERATOR = 'Ninja'
    $env:CARGO_ENCODED_RUSTFLAGS = $null
}
$env:CARGO_PROFILE_DEV_DEBUG = '0'
# Compile upstream dependencies quickly; EarthCraft's core, data, and render
# crates keep their per-package opt-level=2 profile in Cargo.toml.
$env:CARGO_PROFILE_DEV_OPT_LEVEL = '0'
cargo build --locked --jobs 2
if ($LASTEXITCODE -ne 0) { throw 'EarthCraft build failed. See compiler output.' }
