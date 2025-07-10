#!/bin/bash

# Copy built binary from volume to Windows filesystem for flashing
echo "Copying binary to Windows target directory..."

# Create target directory structure if it doesn't exist
mkdir -p /tmp/host-workspace/target/xtensa-esp32-espidf/debug/

# Copy the binary
cp target/xtensa-esp32-espidf/debug/pup /tmp/host-workspace/target/xtensa-esp32-espidf/debug/

echo "Binary copied! You can now flash from Windows PowerShell:"
echo "cd C:/esp/pup"
echo "espflash flash --port COM5 --monitor target/xtensa-esp32-espidf/debug/pup" 