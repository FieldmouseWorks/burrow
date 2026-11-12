// The dashboard's service worker (src/assets.rs fills in the version and
// the shell). It caches the shell, which is the same for everyone and holds
// nothing of the machine's, and never a page or an answer with data in it:
// those always come from the network, and when the network isn't there a
// page is the offline one instead.
"use strict";

const VERSION = "@VERSION@";
const SHELL = ["@SHELL@"];
const CACHE = `shell-${VERSION}`;

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE).then((cache) => cache.addAll(SHELL)).then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== "GET" || url.origin !== self.location.origin) {
    return;
  }
  if (SHELL.includes(url.pathname)) {
    event.respondWith(caches.match(url.pathname).then((hit) => hit || fetch(request)));
  } else if (request.mode === "navigate") {
    event.respondWith(
      fetch(request).catch(() =>
        caches.match("/offline").then((page) => page || Response.error()),
      ),
    );
  }
});
