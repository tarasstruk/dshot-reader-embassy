"""Motor 4 sweep: MSP_SET_MOTOR on the FC, telemetry from the FC (MSP) and from the RP2040 (USB text).

The FC must be disarmed. Motors 1-3 stay at 1000. Motor 4 always goes back to 1000 at the end.
"""
import os, sys, time, termios, select, struct, subprocess

FC, RP = sys.argv[1], sys.argv[2]
VALUES = [int(v) for v in sys.argv[3].split(",")]
SETTLE = 1.5

def open_raw(dev):
    fd = os.open(dev, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    a = termios.tcgetattr(fd)
    a[0] = 0; a[1] = 0; a[3] = 0
    a[2] |= termios.CLOCAL | termios.CREAD
    a[4] = a[5] = termios.B115200
    termios.tcsetattr(fd, termios.TCSANOW, a)
    termios.tcflush(fd, termios.TCIOFLUSH)
    return fd

fc = open_raw(FC)

def msp(cmd, payload=b"", timeout=1.0):
    ck = len(payload) ^ cmd
    for b in payload: ck ^= b
    os.write(fc, b"$M<" + bytes([len(payload), cmd]) + payload + bytes([ck]))
    buf = b""; end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([fc], [], [], 0.05)
        if r:
            try: buf += os.read(fc, 1024)
            except BlockingIOError: pass
        i = buf.find(b"$M")
        if i >= 0 and len(buf) >= i + 5:
            size, c = buf[i + 3], buf[i + 4]
            if len(buf) >= i + 6 + size:
                if buf[i + 2:i + 3] == b"!":
                    return None
                data = buf[i + 5:i + 5 + size]
                if c == cmd:
                    return data
                buf = buf[i + 2:]
    return None

def set_motor4(v):
    vals = [1000, 1000, 1000, v, 1000, 1000, 1000, 1000]
    return msp(214, struct.pack("<8H", *vals)) is not None   # MSP_SET_MOTOR

def fc_motor4():
    p = msp(139)                                              # MSP_MOTOR_TELEMETRY
    rpm, inv, temp, volt, cur, cons = struct.unpack_from("<IHBHHH", p, 1 + 3 * 13)
    out = msp(104)                                            # MSP_MOTOR
    return {"out": struct.unpack_from("<4H", out, 0)[3], "rpm": rpm, "invalid_%": inv / 100, "temp": temp, "cur": cur / 100}

def rp_line():
    # Separate process with a timeout: opening the RP2040 port has hung before.
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

def expected_dshot(v):
    return 0 if v <= 1000 else 48 + (v - 1001) * 1999 // 999

try:
    for v in VALUES:
        ok = set_motor4(v)
        time.sleep(SETTLE)
        t = fc_motor4()
        line = rp_line()
        d = expected_dshot(v)
        erpm = 0 if d < 48 else 1000 + d * 20
        print(f"set {v} (ok={ok}) expect thr={d} eRPM≈{erpm} RPM≈{erpm // 7} | FC: {t} | RP2040: {line}", flush=True)
finally:
    set_motor4(1000)
    time.sleep(0.5)
    print("back to 1000:", fc_motor4(), flush=True)
    os.close(fc)
