#!/usr/bin/env python3
"""本地模拟飞书开放平台：用于在没有真实飞书应用时测试 IM 远程控制。

只用标准库。一个端口同时提供：
- 开放平台接口：tenant_access_token、bot/v3/info、im/v1/messages（回复 / 发送 / 更新卡片）、im/v1/files、im/v1/images
- 长连接：POST /callback/ws/endpoint 返回 ws 地址，/ws 上按 pbbp2.Frame 收发（ping/pong、事件、ACK）
- 测试控制：
    POST /mock/send   {"text": "...", "chat_id": "oc_1", "chat_type": "p2p", "sender": "ou_user", "mention": false, "fragments": 1}
                      通过长连接推送一条 im.message.receive_v1 事件；msg_type=image/file 时用 content 指定 image_key / file_key
    POST /mock/card   {"value": {...}, "message_id": "om_bot_3", "chat_id": "oc_1", "operator": "ou_user"}
                      推送一次卡片按钮回调（card.action.trigger）
    GET  /mock/log    机器人发出的所有调用（回复、卡片更新、上传）以及收到的 ACK
    POST /mock/reset  清空记录

用法：
    python3 dev/mock_feishu.py            # 默认 8778 端口
    FEISHU_API_BASE=http://127.0.0.1:8778 ./server/target/debug/walioffice
渠道里 App ID 随意，App Secret 填 mock-secret（其他值会返回鉴权失败）。
"""

import asyncio
import base64
import hashlib
import json
import os
import struct
import time
import uuid

PORT = int(os.environ.get("MOCK_FEISHU_PORT", "8778"))
SECRET = "mock-secret"
BOT_OPEN_ID = "ou_mock_bot"
SERVICE_ID = 7

LOG = []
CLIENTS = set()
COUNTER = {"msg": 0}


def record(kind, **data):
    LOG.append({"kind": kind, "at": time.time(), **data})


# ───────────── protobuf：pbbp2.Frame ─────────────

def varint(v):
    out = bytearray()
    v &= (1 << 64) - 1
    while v >= 0x80:
        out.append((v & 0x7F) | 0x80)
        v >>= 7
    out.append(v)
    return bytes(out)


def field_varint(tag, v):
    return varint(tag << 3) + varint(v)


def field_bytes(tag, b):
    return varint((tag << 3) | 2) + varint(len(b)) + b


def encode_frame(method, headers, payload=b"", seq_id=0, log_id=0):
    out = field_varint(1, seq_id) + field_varint(2, log_id) + field_varint(3, SERVICE_ID) + field_varint(4, method)
    for k, v in headers.items():
        out += field_bytes(5, field_bytes(1, k.encode()) + field_bytes(2, v.encode()))
    if payload:
        out += field_bytes(8, payload)
    return out


def read_varint(buf, pos):
    v, shift = 0, 0
    while True:
        b = buf[pos]
        pos += 1
        v |= (b & 0x7F) << shift
        if b < 0x80:
            return v, pos
        shift += 7


def decode_frame(buf):
    frame = {"headers": {}, "payload": b""}
    pos = 0
    while pos < len(buf):
        key, pos = read_varint(buf, pos)
        tag, wire = key >> 3, key & 7
        if wire == 0:
            v, pos = read_varint(buf, pos)
            frame[{1: "seq_id", 2: "log_id", 3: "service", 4: "method"}.get(tag, str(tag))] = v
        elif wire == 2:
            n, pos = read_varint(buf, pos)
            b = buf[pos:pos + n]
            pos += n
            if tag == 5:
                inner, ip, kv = b, 0, {}
                while ip < len(inner):
                    k2, ip = read_varint(inner, ip)
                    n2, ip = read_varint(inner, ip)
                    kv[k2 >> 3] = inner[ip:ip + n2].decode()
                    ip += n2
                frame["headers"][kv.get(1, "")] = kv.get(2, "")
            elif tag == 8:
                frame["payload"] = b
        else:
            raise ValueError("unexpected wire type")
    return frame


# ───────────── WebSocket ─────────────

async def ws_send(writer, data, opcode=2):
    header = bytes([0x80 | opcode])
    n = len(data)
    if n < 126:
        header += bytes([n])
    elif n < 65536:
        header += bytes([126]) + struct.pack(">H", n)
    else:
        header += bytes([127]) + struct.pack(">Q", n)
    writer.write(header + data)
    await writer.drain()


async def ws_recv(reader):
    b1, b2 = await reader.readexactly(2)
    opcode = b1 & 0x0F
    n = b2 & 0x7F
    if n == 126:
        n = struct.unpack(">H", await reader.readexactly(2))[0]
    elif n == 127:
        n = struct.unpack(">Q", await reader.readexactly(8))[0]
    mask = await reader.readexactly(4) if b2 & 0x80 else b"\0\0\0\0"
    data = bytearray(await reader.readexactly(n))
    for i in range(n):
        data[i] ^= mask[i % 4]
    return opcode, bytes(data)


async def handle_ws(reader, writer, headers):
    key = headers.get("sec-websocket-key", "")
    accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
    writer.write(
        ("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
         f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode()
    )
    await writer.drain()
    CLIENTS.add(writer)
    record("ws_connected")
    try:
        while True:
            opcode, data = await ws_recv(reader)
            if opcode == 8:
                break
            if opcode == 9:
                await ws_send(writer, data, opcode=10)
                continue
            if opcode != 2:
                continue
            frame = decode_frame(data)
            if frame.get("method", 0) == 0 and frame["headers"].get("type") == "ping":
                record("ping", seq_id=frame.get("seq_id"), service=frame.get("service"))
                pong = encode_frame(0, {"type": "pong"}, json.dumps({"PingInterval": 30}).encode())
                await ws_send(writer, pong)
            elif frame.get("method") == 1:
                record("ack", message_id=frame["headers"].get("message_id"), seq=frame["headers"].get("seq"),
                       payload=json.loads(frame["payload"] or b"null"), biz_rt=frame["headers"].get("biz_rt"))
    except (asyncio.IncompleteReadError, ConnectionError):
        pass
    finally:
        CLIENTS.discard(writer)
        record("ws_closed")


async def push_event(body):
    event_id = "ev_" + uuid.uuid4().hex[:12]
    message_id = "om_" + uuid.uuid4().hex[:12]
    text = body.get("text", "")
    mentions = []
    if body.get("mention"):
        text = "@_user_1 " + text
        mentions = [{"key": "@_user_1", "id": {"open_id": body.get("mention_open_id", BOT_OPEN_ID)}, "name": "模拟助手"}]
    event = {
        "schema": "2.0",
        "header": {"event_id": event_id, "event_type": "im.message.receive_v1", "create_time": str(int(time.time() * 1000)),
                   "token": "", "app_id": "cli_mock", "tenant_key": "mock"},
        "event": {
            "sender": {"sender_id": {"open_id": body.get("sender", "ou_user")}, "sender_type": "user", "tenant_key": "mock"},
            "message": {
                "message_id": message_id, "chat_id": body.get("chat_id", "oc_1"), "chat_type": body.get("chat_type", "p2p"),
                "message_type": body.get("msg_type", "text"), "content": json.dumps(body.get("content") or {"text": text}, ensure_ascii=False),
                "mentions": mentions, "create_time": str(int(time.time() * 1000)),
            },
        },
    }
    payload = json.dumps(event, ensure_ascii=False).encode()
    n = max(1, int(body.get("fragments", 1)))
    size = -(-len(payload) // n)
    parts = [payload[i * size:(i + 1) * size] for i in range(n)]
    # 分片故意倒序发送，检验重组
    frame_id = "fm_" + uuid.uuid4().hex[:8]
    for seq in reversed(range(n)):
        frame = encode_frame(1, {"type": "event", "message_id": frame_id, "sum": str(n), "seq": str(seq), "trace_id": "t"}, parts[seq])
        for writer in list(CLIENTS):
            await ws_send(writer, frame)
        # 重复推送同一事件（检验去重）
    if body.get("duplicate"):
        frame = encode_frame(1, {"type": "event", "message_id": frame_id + "b", "sum": "1", "seq": "0"}, payload)
        for writer in list(CLIENTS):
            await ws_send(writer, frame)
    return {"clients": len(CLIENTS), "event_id": event_id, "message_id": message_id}


async def push_card_action(body):
    event_id = "ev_" + uuid.uuid4().hex[:12]
    event = {
        "schema": "2.0",
        "header": {"event_id": event_id, "event_type": "card.action.trigger", "token": "", "app_id": "cli_mock"},
        "event": {
            "operator": {"open_id": body.get("operator", "ou_user")},
            "action": {"tag": "button", "value": body.get("value", {})},
            "context": {"open_message_id": body.get("message_id", ""), "open_chat_id": body.get("chat_id", "oc_1")},
        },
    }
    frame = encode_frame(1, {"type": "event", "message_id": "fm_" + uuid.uuid4().hex[:8], "sum": "1", "seq": "0"}, json.dumps(event).encode())
    for writer in list(CLIENTS):
        await ws_send(writer, frame)
    return {"clients": len(CLIENTS), "event_id": event_id}


# 消息里的资源：image_key / file_key 前缀决定返回什么
PNG_1PX = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
CSV_SALES = "月份,区域,销售额\n1月,华东,120\n1月,华南,90\n2月,华东,150\n2月,华南,110\n3月,华东,170\n3月,华南,95\n".encode()


def binary(writer, content_type, data):
    writer.write(f"HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {len(data)}\r\nConnection: close\r\n\r\n".encode() + data)


# ───────────── HTTP ─────────────

def respond(writer, status, obj):
    data = json.dumps(obj, ensure_ascii=False).encode()
    writer.write(f"HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {len(data)}\r\nConnection: close\r\n\r\n".encode() + data)


def next_message_id():
    COUNTER["msg"] += 1
    return f"om_bot_{COUNTER['msg']}"


def authed(headers):
    return headers.get("authorization") == "Bearer t-mock"


async def handle(reader, writer):
    try:
        line = (await reader.readline()).decode().strip()
        if not line:
            return
        method, target, _ = line.split(" ", 2)
        headers = {}
        while True:
            h = (await reader.readline()).decode().strip()
            if not h:
                break
            k, v = h.split(":", 1)
            headers[k.strip().lower()] = v.strip()
        if headers.get("upgrade", "").lower() == "websocket":
            await handle_ws(reader, writer, headers)
            return
        body = await reader.readexactly(int(headers.get("content-length", "0") or 0))
        path = target.split("?")[0]
        is_json = "json" in headers.get("content-type", "")
        data = json.loads(body or b"{}") if is_json else {}

        if path == "/open-apis/auth/v3/tenant_access_token/internal":
            if data.get("app_secret") != SECRET:
                return respond(writer, 200, {"code": 10014, "msg": "app secret invalid"})
            return respond(writer, 200, {"code": 0, "tenant_access_token": "t-mock", "expire": 7200})
        if path == "/callback/ws/endpoint":
            if data.get("AppSecret") != SECRET:
                return respond(writer, 200, {"code": 1000040351, "msg": "incorrect app secret"})
            url = f"ws://127.0.0.1:{PORT}/ws?device_id=mock&service_id={SERVICE_ID}"
            return respond(writer, 200, {"code": 0, "data": {"URL": url, "ClientConfig": {"PingInterval": 30, "ReconnectCount": -1}}})
        if path.startswith("/mock/"):
            if path == "/mock/send":
                return respond(writer, 200, await push_event(data))
            if path == "/mock/card":
                return respond(writer, 200, await push_card_action(data))
            if path == "/mock/log":
                return respond(writer, 200, {"log": LOG, "clients": len(CLIENTS)})
            if path == "/mock/reset":
                LOG.clear()
                return respond(writer, 200, {"ok": True})
        if not authed(headers):
            return respond(writer, 200, {"code": 99991663, "msg": "invalid access token"})
        if path == "/open-apis/bot/v3/info":
            return respond(writer, 200, {"code": 0, "bot": {"app_name": "模拟助手", "open_id": BOT_OPEN_ID}})
        if path.startswith("/open-apis/im/v1/chats/"):
            return respond(writer, 200, {"code": 0, "data": {"name": "模拟项目群", "chat_id": path.rsplit("/", 1)[1]}})
        if path.startswith("/open-apis/im/v1/messages") and "/resources/" in path:
            key = path.rsplit("/", 1)[1]
            record("download", key=key, query=target.split("?", 1)[1] if "?" in target else "")
            if key.startswith("missing"):
                return respond(writer, 404, {"code": 234003, "msg": "File not in msg."})
            if key.startswith("img"):
                return binary(writer, "image/png", PNG_1PX)
            return binary(writer, "application/octet-stream", CSV_SALES)
        if path.startswith("/open-apis/im/v1/messages"):
            parts = path.split("/")
            if method == "POST" and path.endswith("/reply"):
                mid = next_message_id()
                record("reply", to=parts[5], msg_type=data.get("msg_type"), content=json.loads(data.get("content", "{}")), message_id=mid)
                return respond(writer, 200, {"code": 0, "data": {"message_id": mid}})
            if method == "POST":
                mid = next_message_id()
                record("send", receive_id=data.get("receive_id"), msg_type=data.get("msg_type"), content=json.loads(data.get("content", "{}")), message_id=mid)
                return respond(writer, 200, {"code": 0, "data": {"message_id": mid}})
            if method == "PATCH":
                record("update", message_id=parts[5], content=json.loads(data.get("content", "{}")))
                return respond(writer, 200, {"code": 0, "data": {}})
        if path in ("/open-apis/im/v1/files", "/open-apis/im/v1/images"):
            text = body.decode("utf-8", "ignore")
            name = text.split('filename="', 1)[1].split('"', 1)[0] if 'filename="' in text else ""
            kind = "image" if path.endswith("images") else "file"
            key = f"{kind}_key_{uuid.uuid4().hex[:6]}"
            record("upload", upload=kind, file_name=name, bytes=len(body))
            return respond(writer, 200, {"code": 0, "data": {f"{kind}_key": key}})
        respond(writer, 404, {"code": 404, "msg": f"mock: {method} {path} 未实现"})
    except Exception as e:  # noqa: BLE001
        try:
            respond(writer, 500, {"code": 500, "msg": str(e)})
        except Exception:  # noqa: BLE001
            pass
    finally:
        try:
            await writer.drain()
            writer.close()
        except Exception:  # noqa: BLE001
            pass


async def main():
    server = await asyncio.start_server(handle, "127.0.0.1", PORT)
    print(f"mock feishu listening on http://127.0.0.1:{PORT}", flush=True)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
