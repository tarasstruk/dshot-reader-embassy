"""Read-only MSP v1 queries to a Betaflight FC (no CLI, nothing is changed)."""
import os, sys, time, termios, select, struct

dev = sys.argv[1]
fd = os.open(dev, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
a = termios.tcgetattr(fd)
a[0] = 0; a[1] = 0; a[3] = 0
a[2] |= termios.CLOCAL | termios.CREAD
a[4] = a[5] = termios.B115200
termios.tcsetattr(fd, termios.TCSANOW, a)
termios.tcflush(fd, termios.TCIOFLUSH)

def msp(cmd, timeout=1.0):
    os.write(fd, b"$M<" + bytes([0, cmd, cmd]))  # size 0, checksum = size ^ cmd
    buf = b""; end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try: buf += os.read(fd, 1024)
            except BlockingIOError: pass
        i = buf.find(b"$M")
        if i >= 0 and len(buf) >= i + 5:
            size, c = buf[i + 3], buf[i + 4]
            if len(buf) >= i + 6 + size:
                if buf[i + 2:i + 3] == b"!":
                    return None
                payload = buf[i + 5:i + 5 + size]
                ck = size ^ c
                for b in payload: ck ^= b
                if c == cmd and ck == buf[i + 5 + size]:
                    return payload
                buf = buf[i + 2:]
    return None

out = {}
p = msp(1);   out["api"] = f"{p[0]}.{p[1]}.{p[2]}" if p else None          # MSP_API_VERSION: proto, major, minor
p = msp(2);   out["variant"] = p.decode() if p else None                     # MSP_FC_VARIANT
p = msp(3);   out["version"] = ".".join(map(str, p)) if p else None          # MSP_FC_VERSION
p = msp(4)                                                                   # MSP_BOARD_INFO
if p:
    out["board_id"] = p[:4].decode(errors="replace")
    try:
        n = p[8]; out["target"] = p[9:9 + n].decode(errors="replace")
    except IndexError: pass
p = msp(5)                                                                   # MSP_BUILD_INFO
if p: out["build"] = p[:11].decode(errors="replace") + " " + p[11:19].decode(errors="replace")
p = msp(150)                                                                 # MSP_STATUS_EX
if p:
    cycle, i2c_err, sensors, flags = struct.unpack_from("<HHHI", p, 0)
    out["cycle_us"] = cycle; out["i2c_errors"] = i2c_err
    out["sensors"] = [n for b, n in enumerate(["acc", "baro", "mag", "gps", "sonar", "gyro"]) if sensors >> b & 1]
    out["cpu_load_%"] = struct.unpack_from("<H", p, 11)[0]
    # arming disable flags follow the box flags
    k = 15; nbox = p[k]; k += 1 + nbox
    if len(p) >= k + 5:
        out["arming_disable_count"] = p[k]
        out["arming_disable_flags"] = hex(struct.unpack_from("<I", p, k + 1)[0])
p = msp(110)                                                                 # MSP_ANALOG
if p:
    out["vbat_V"] = p[0] / 10
    if len(p) >= 9: out["amperage_A"] = struct.unpack_from("<h", p, 5)[0] / 100
    if len(p) >= 11: out["vbat_V_precise"] = struct.unpack_from("<H", p, 9)[0] / 100
p = msp(104)                                                                 # MSP_MOTOR
if p: out["motor_outputs"] = list(struct.unpack_from(f"<{min(len(p)//2, 8)}H", p, 0))[:4]
p = msp(139)                                                                 # MSP_MOTOR_TELEMETRY
if p:
    n = p[0]; tel = []
    for m in range(n):
        rpm, inv, temp, volt, cur, cons = struct.unpack_from("<IHBHHH", p, 1 + m * 13)
        tel.append({"motor": m + 1, "rpm": rpm, "invalid_%": inv / 100, "temp": temp, "volt": volt / 100, "cur": cur / 100})
    out["motor_telemetry"] = tel
os.close(fd)
for k, v in out.items():
    print(f"{k}: {v}")
