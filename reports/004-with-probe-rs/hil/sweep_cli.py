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

def rp_line():
    code = (
        "import os,time,select;fd=os.open(%r,os.O_RDONLY|os.O_NOCTTY|os.O_NONBLOCK);b=b'';e=time.time()+1.2\n"
        "while time.time()<e:\n r,_,_=select.select([fd],[],[],0.1)\n if r:\n  try:b+=os.read(fd,4096)\n  except BlockingIOError:pass\n"
        "print(b.decode(errors='replace'))" % RP)
    try:
        r = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=4)
        lines = [l.strip() for l in r.stdout.replace("\r", "").split("\n") if "n=" in l]
        return lines[-1] if lines else "(no line)"
    except subprocess.TimeoutExpired:
        return "(RP2040 port open timed out)"

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
