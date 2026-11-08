#!/usr/bin/env python3
"""Dedicated lab BGP source. No router CLI/config writes; independent wire encoder."""
import fcntl
import http.server
import ipaddress
import json
import socket
import struct
import threading
import time

SESSIONS = [("default", "eth1", "10.206.0.2", "10.206.0.1", "10.77.1.0/24"),
            ("blue", "eth2", "10.206.1.2", "10.206.1.1", "10.77.2.0/24"),
            ("srl", "eth3", "10.206.2.2", "10.206.2.1", "10.77.1.0/24")]
STATE = {}
LOCK = threading.RLock()

def frame(kind, body=b""):
    return b"\xff" * 16 + struct.pack("!HB", 19 + len(body), kind) + body

def nlri(prefix):
    net = ipaddress.ip_network(prefix)
    return bytes([net.prefixlen]) + net.network_address.packed[:(net.prefixlen+7)//8]

def update(src, prefix, present, med):
    prefix = nlri(prefix)
    if not present:
        return frame(2, struct.pack("!H", len(prefix)) + prefix + b"\0\0")
    # Four-octet ASN capability negotiated; wire AS_PATH is independently encoded.
    attrs = b"\x40\x01\x01\x00" + b"\x40\x02\x06\x02\x01" + struct.pack("!I",65002)
    attrs += b"\x40\x03\x04" + socket.inet_aton(src)
    attrs += b"\x80\x04\x04" + struct.pack("!I",med)
    return frame(2, b"\0\0" + struct.pack("!H",len(attrs)) + attrs + prefix)

def setup(iface, address):
    with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as s:
        req = lambda ip: struct.pack("16sH2s4s8s",iface.encode(),socket.AF_INET,b"\0\0",socket.inet_aton(ip),b"\0"*8)
        fcntl.ioctl(s,0x8916,req(address))
        fcntl.ioctl(s,0x891c,req("255.255.255.252"))
        flags=struct.unpack("16sH14s",fcntl.ioctl(s,0x8913,struct.pack("16sH14s",iface.encode(),0,b"\0"*14)))[1]
        fcntl.ioctl(s,0x8914,struct.pack("16sH14s",iface.encode(),flags|1,b"\0"*14))

def session(name,iface,src,dst,prefix):
    setup(iface,src)
    while True:
        sock = None
        try:
            sock=socket.socket(); sock.bind((src,0)); sock.settimeout(10); sock.connect((dst,179))
            caps=b"\x02\x06\x41\x04"+struct.pack("!I",65002)
            sock.sendall(frame(1,struct.pack("!BHH4sB",4,65002,90,socket.inet_aton("192.0.2.2"),len(caps))+caps))
            # Wait for the peer OPEN before entering Established.
            def exact(length):
                data=b""
                while len(data)<length:
                    chunk=sock.recv(length-len(data))
                    if not chunk: raise ConnectionError("EOF before OPEN")
                    data+=chunk
                return data
            header=exact(19)
            length,kind=struct.unpack("!HB",header[16:19])
            if header[:16]!=b"\xff"*16 or not 19<=length<=4096:
                raise RuntimeError("invalid BGP header")
            exact(length-19)
            if kind!=1: raise RuntimeError("peer did not send OPEN")
            sock.sendall(frame(4))
            with LOCK:
                STATE[name].update(socket=sock,established=True,error=None)
                sock.sendall(update(src,prefix,STATE[name]["present"],STATE[name]["med"]))
                sock.sendall(frame(2,b"\0\0\0\0"))
            sock.settimeout(5)
            while True:
                try:
                    if not sock.recv(65536):raise ConnectionError("EOF")
                except socket.timeout:
                    with LOCK:sock.sendall(frame(4))
        except Exception as exc:
            with LOCK:STATE[name].update(socket=None,established=False,error=type(exc).__name__)
            if sock:sock.close()
            time.sleep(1)

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self,*args): pass
    def do_GET(self):
        with LOCK:body=json.dumps({k:{a:b for a,b in v.items() if a!="socket"} for k,v in STATE.items()}).encode()
        self.send_response(200);self.end_headers();self.wfile.write(body)
    def do_POST(self):
        try:
            req=json.loads(self.rfile.read(min(int(self.headers.get("Content-Length",0)),4096)))
            with LOCK:
                names=list(STATE) if req.get("session")=="all" else [req["session"]]
                for name in names:
                    v=STATE[name];v["present"]=bool(req.get("present",True));v["med"]=int(req.get("med",0))
                    if v["socket"]:v["socket"].sendall(update(v["src"],v["prefix"],v["present"],v["med"]))
            self.do_GET()
        except Exception:
            self.send_error(400,"invalid lab command")

if __name__=="__main__":
    for name,iface,src,dst,prefix in SESSIONS:
        STATE[name]={"src":src,"prefix":prefix,"present":True,"med":0,"established":False,"socket":None}
    for args in SESSIONS:threading.Thread(target=session,args=args,daemon=True).start()
    http.server.ThreadingHTTPServer(("0.0.0.0",19000),Handler).serve_forever()
