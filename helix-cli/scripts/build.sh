#!/bin/bash
set -e

# Clean previous builds
rm -rf releases/
mkdir -p releases/

# Build for different targets
targets=("x86_64-unknown-linux-gnu" "x86_64-pc-windows-gnu" "x86_64-apple-darwin" "aarch64-apple-darwin")

for target in "${targets[@]}"; do
    echo "Building for $target..."
    
    # Install target if not present
    rustup target add $target || true
    
    # Build
    cargo build --release --target $target
    
    # Package
    if [[ $target == *"windows"* ]]; then
        cp target/$target/release/helix.exe releases/helix-$target.exe
        (cd releases && zip helix-$target.zip helix-$target.exe)
        rm releases/helix-$target.exe
    else
        cp target/$target/release/helix releases/helix-$target
        (cd releases && tar -czf helix-$target.tar.gz helix-$target)
        rm releases/helix-$target
    fi
    
    echo "✓ Built $target"
done

echo "All binaries built in releases/ directory"
ls -la releases/