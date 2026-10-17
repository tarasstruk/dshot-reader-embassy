"""Motor 4 sweep through the Betaflight CLI: `motor 3 <v>` + `dshot_telemetry_info`,
plus the RP2040 USB line. Motor 4 goes back to 1000, then `exit` (the FC reboots)."""
import os, sys, time, termios, select, subprocess, re

FC, RP = sys.argv[1], sys.argv[2]
VALUES = [int(v) for v in sys.argv[3].split(",")]

fd = os.open(FC, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
a = termios.tcgetattr(fd)
a[0] = 0; a[1] = 0; a[3] = 0
a[2] |= termios.CLOCAL | termios.CREAD
a[4] = a[5] = termios.B115200
termios.tcsetattr(fd, termios.TCSANOW, a)
termios.tcflush(fd, termios.TCIOFLUSH)

def read_for(secs, until=None):
    buf = b""; end = time.time() + secs
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try: buf += os.read(fd, 4096)
            except BlockingIOError: pass
            if until and buf.rstrip().endswith(until):
                break
    return buf.decode(errors="replace").replace("\r", "")

def cli(cmd, secs=2.0):
    os.write(fd, cmd.encode() + b"\n")
    return read_for(secs, until=b"#")

# The RP2040 port is opened once, by a reader process that streams its lines to us.
# Closing that port can hang for tens of seconds (findings-after-migration-stage-2.md),
# so the reader is never closed: it is killed at the end.
READER = (
    "import os,sys,select\n"
    "fd=os.open(%r,os.O_RDONLY|os.O_NOCTTY|os.O_NONBLOCK)\n"
    "while True:\n"
    " r,_,_=select.select([fd],[],[],0.5)\n"
    " if r:\n"
    "  try: sys.stdout.write(os.read(fd,4096).decode(errors='replace')); sys.stdout.flush()\n"
    "  except BlockingIOError: pass\n" % RP)
reader = subprocess.Popen([sys.executable, "-c", READER], stdout=subprocess.PIPE)
os.set_blocking(reader.stdout.fileno(), False)
rp_buf = ""

def rp_line():
    """The newest complete line from the RP2040 (waits up to 1.2 s for a fresh one)."""
    global rp_buf
    end = time.time() + 1.2
    while time.time() < end:
        try:
            chunk = reader.stdout.read()
        except BlockingIOError:
            chunk = None
        if chunk:
            rp_buf += chunk.decode(errors="replace").replace("\r", "")
        time.sleep(0.1)
    lines = [l.strip() for l in rp_buf.split("\n")[:-1] if "n=" in l]
    rp_buf = rp_buf.split("\n")[-1]
    return lines[-1] if lines else "(no line)"

def motor4_row(text):
    for l in text.split("\n"):
        if re.match(r"\s*4\s", l):
            return " ".join(l.split())
    return "(no row)"

os.write(fd, b"#")
banner = read_for(2.0, until=b"#")
print("CLI:", banner.strip().split("\n")[-1] if banner.strip() else "(no prompt)", flush=True)
try:
    for v in VALUES:
        out = cli(f"motor 3 {v}")
        m = re.search(r"motor 3: (\d+)", out)
        time.sleep(1.5)
        tel = cli("dshot_telemetry_info", 2.0)
        print(f"motor 3 {v} -> {m.group(1) if m else out.strip()!r} | FC: {motor4_row(tel)} | RP2040: {rp_line()}", flush=True)
finally:
    out = cli("motor 3 1000")
    m = re.search(r"motor 3: (\d+)", out)
    time.sleep(1.0)
    tel = cli("dshot_telemetry_info", 2.0)
    print(f"motor 3 1000 -> {m.group(1) if m else out.strip()!r} | FC: {motor4_row(tel)}", flush=True)
    print("telemetry header:", [l for l in tel.split("\n") if l.startswith("Motor")][:1], flush=True)
    os.write(fd, b"exit\n")
    print("exit:", read_for(1.5).strip()[-80:], flush=True)
    os.close(fd)
    reader.kill()
