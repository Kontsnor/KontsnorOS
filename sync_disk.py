#!/usr/bin/env python3
import os
import subprocess
import tempfile

PROJECT_DIR = "/home/kontsnor/Projects/KontsnorOS"
DISK_IMG = os.path.join(PROJECT_DIR, "disk.img")

def main():
    commands = []
    
    # Base files
    for base_file in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]:
        full_path = os.path.join(PROJECT_DIR, base_file)
        if os.path.exists(full_path):
            target = f"/src/KontsnorOS/{base_file}"
            commands.append(f"rm {target}")
            commands.append(f"write {full_path} {target}")

    # Subdirectories to sync
    for subdir in ["kernel", "driver-sdk", "tools"]:
        src_root = os.path.join(PROJECT_DIR, subdir)
        for root, dirs, files in os.walk(src_root):
            # Create directories
            for d in sorted(dirs):
                full_dir = os.path.join(root, d)
                rel_dir = os.path.relpath(full_dir, PROJECT_DIR)
                commands.append(f"mkdir /src/KontsnorOS/{rel_dir}")
            # Overwrite files
            for f in sorted(files):
                full_file = os.path.join(root, f)
                rel_file = os.path.relpath(full_file, PROJECT_DIR)
                target = f"/src/KontsnorOS/{rel_file}"
                commands.append(f"rm {target}")
                commands.append(f"write {full_file} {target}")

    print(f"Generated {len(commands)} debugfs commands.")
    
    with tempfile.NamedTemporaryFile("w", delete=False) as tf:
        cmd_file = tf.name
        tf.write("\n".join(commands) + "\n")

    try:
        print("Executing debugfs to sync source files to disk.img...")
        res = subprocess.run(
            ["debugfs", "-w", DISK_IMG, "-f", cmd_file],
            capture_output=True,
            text=True
        )
        print(f"debugfs exited with code {res.returncode}")
        if res.stderr:
            # Filter out expected "File exists" for mkdir or "File not found" for rm
            errors = [line for line in res.stderr.splitlines() if "File not found by ext2_lookup" not in line and "File exists by ext2_lookup" not in line]
            if errors:
                print("debugfs stderr:\n" + "\n".join(errors[:20]))
    finally:
        os.remove(cmd_file)

    print("Source sync completed.")

if __name__ == "__main__":
    main()
