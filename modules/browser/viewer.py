"""websockify, serving noVNC, with every response refusing to be framed.

The viewer asks for no password and ExpectOrigin only checks who opens its
WebSocket: a foreign page that framed vnc.html would open it from the
viewer's own origin, and could lay decoys over it to steer the owner's
clicks and keys into their signed-in browser. websockify has no option for
response headers, so its handler gets them here. Arguments are websockify's.
"""

from websockify import websocketproxy

handler = websocketproxy.ProxyRequestHandler
end_headers = handler.end_headers


def framed_nowhere(self):
    self.send_header("Content-Security-Policy", "frame-ancestors 'none'")
    self.send_header("X-Frame-Options", "DENY")
    end_headers(self)


handler.end_headers = framed_nowhere
websocketproxy.websockify_init()
