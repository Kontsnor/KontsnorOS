import subprocess, time, pty, os, select, socket, json, sys

print("Opening PTY...", flush=True)
master, slave = pty.openpty()

cmd = [
    'qemu-system-x86_64',
    '-drive', 'format=raw,file=/home/kontsnor/Projects/KontsnorOS/bios.img',
    '-drive', 'format=raw,file=/home/kontsnor/Projects/KontsnorOS/disk-ubuntu.img,index=1,media=disk',
    '-serial', 'stdio',
    '-display', 'none',
    '-m', '4G',
    '-qmp', 'unix:/tmp/qmp-kontsnor.sock,server,nowait',
    '-enable-kvm', '-cpu', 'host', '-smp', '8',
    '-netdev', 'user,id=net0',
    '-device', 'e1000,netdev=net0',
    '-no-reboot', '-no-shutdown'
]

print("Launching QEMU...", flush=True)
proc = subprocess.Popen(cmd, stdin=slave, stdout=slave, stderr=slave, close_fds=True)
os.close(slave)

buf = b''
start = time.time()
prompt_found = False
while time.time() - start < 60:
    r, _, _ = select.select([master], [], [], 0.2)
    if r:
        try:
            chunk = os.read(master, 4096)
            if not chunk: break
            buf += chunk
            if b'root@kontsnoros:/# ' in buf and b'[init-ubuntu] Spawning' in buf:
                prompt_found = True
                break
        except OSError:
            break

if not prompt_found:
    print("Boot timed out! Output so far:", flush=True)
    print(buf.decode(errors='replace'), flush=True)
    proc.kill()
    sys.exit(1)

print("Boot completed successfully! Waiting 1s before typing...", flush=True)
time.sleep(1.0)

# Flush pending input
while True:
    r, _, _ = select.select([master], [], [], 0.1)
    if r:
        os.read(master, 4096)
    else:
        break

print("Sending: apt-get update", flush=True)
os.write(master, b'apt-get update\n')

# Capture output for up to 60 seconds
out = b''
start = time.time()
while time.time() - start < 60:
    r, _, _ = select.select([master], [], [], 0.5)
    if r:
        try:
            chunk = os.read(master, 4096)
            if not chunk: break
            out += chunk
            sys.stdout.write(chunk.decode(errors='replace'))
            sys.stdout.flush()
            if b'root@kontsnoros:/# ' in out:
                print("\nCommand finished and returned to prompt!", flush=True)
                break
        except OSError:
            break

print("\n--- Checking QMP Monitor ---", flush=True)
qmp = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
qmp.connect('/tmp/qmp-kontsnor.sock')
qf = qmp.makefile('r')
qf.readline()
qmp.sendall(b'{"execute": "qmp_capabilities"}\n')
qf.readline()

def hmp(cmd):
    qmp.sendall(json.dumps({"execute": "human-monitor-command", "arguments": {"command-line": cmd}}).encode() + b'\n')
    while True:
        line = qf.readline()
        if not line: break
        resp = json.loads(line)
        if 'return' in resp:
            return resp['return']

print("VM Status:", hmp("info status").strip(), flush=True)
regs = hmp("info registers -a")
for chunk in regs.split('CPU#'):
    if chunk.strip():
        lines = chunk.strip().splitlines()
        cpu_num = lines[0]
        rip = [l.strip() for l in lines if 'RIP=' in l or 'CR3=' in l or 'RAX=' in l]
        print(f"CPU#{cpu_num}:", rip, flush=True)

proc.kill()
os.close(master)
