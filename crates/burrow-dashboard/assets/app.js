// The dashboard's one script: the passkey ceremonies on the sign-in and
// enrolment pages, the service worker, and saying when the device is
// offline. Pages work out what to do from their data-* attributes; there's
// no script in the pages themselves (the CSP allows none).
"use strict";

(() => {
  const bytes = (b64url) => {
    const b64 = b64url.replace(/-/g, "+").replace(/_/g, "/");
    const raw = atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4));
    return Uint8Array.from(raw, (c) => c.charCodeAt(0));
  };
  const b64url = (buffer) =>
    btoa(String.fromCharCode(...new Uint8Array(buffer)))
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");

  // What the server sends, as navigator.credentials wants it.
  const creationOptions = (o) => ({
    ...o,
    challenge: bytes(o.challenge),
    user: { ...o.user, id: bytes(o.user.id) },
    excludeCredentials: (o.excludeCredentials || []).map((c) => ({ ...c, id: bytes(c.id) })),
  });
  const requestOptions = (o) => ({
    ...o,
    challenge: bytes(o.challenge),
    allowCredentials: (o.allowCredentials || []).map((c) => ({ ...c, id: bytes(c.id) })),
  });

  // What navigator.credentials gives, as the server reads it.
  const registration = (c) => ({
    id: c.id,
    rawId: b64url(c.rawId),
    type: c.type,
    response: {
      attestationObject: b64url(c.response.attestationObject),
      clientDataJSON: b64url(c.response.clientDataJSON),
      transports: c.response.getTransports ? c.response.getTransports() : undefined,
    },
    extensions: c.getClientExtensionResults(),
  });
  const assertion = (c) => ({
    id: c.id,
    rawId: b64url(c.rawId),
    type: c.type,
    response: {
      authenticatorData: b64url(c.response.authenticatorData),
      clientDataJSON: b64url(c.response.clientDataJSON),
      signature: b64url(c.response.signature),
      userHandle: c.response.userHandle ? b64url(c.response.userHandle) : null,
    },
    extensions: c.getClientExtensionResults(),
  });

  const post = async (url, body) => {
    const response = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      credentials: "same-origin",
      body: JSON.stringify(body),
    });
    const answer = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(answer.error || `The dashboard answered ${response.status}.`);
    }
    return answer;
  };

  const say = (form, text) => {
    const status = form.querySelector("[data-status]");
    status.textContent = text;
    status.hidden = !text;
  };

  const why = (err) => {
    if (err && err.name === "NotAllowedError") {
      return "The passkey prompt was closed or timed out. Try again when you're ready.";
    }
    if (err && err.name === "InvalidStateError") {
      return "This device has a passkey here already. Sign in with it instead.";
    }
    if (err instanceof TypeError) {
      return "The dashboard can't be reached. Check the connection and try again.";
    }
    return (err && err.message) || "Something went wrong. Try again.";
  };

  // A form that runs a ceremony: start, ask the authenticator, finish.
  const ceremony = (form, run) => {
    if (!window.PublicKeyCredential) {
      say(form, "This browser can't use passkeys.");
      form.querySelector("button").disabled = true;
      return;
    }
    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const button = form.querySelector("button[type=submit]");
      button.disabled = true;
      say(form, "");
      form.setAttribute("aria-busy", "true");
      try {
        const done = await run(new FormData(form));
        location.assign(done.next || "/");
      } catch (err) {
        say(form, why(err));
        button.disabled = false;
      } finally {
        form.removeAttribute("aria-busy");
      }
    });
  };

  const login = document.querySelector("form[data-login]");
  if (login) {
    ceremony(login, async () => {
      const start = await post("/login/start", {});
      const credential = await navigator.credentials.get({
        publicKey: requestOptions(start.options.publicKey),
      });
      return post("/login/finish", { ceremony: start.ceremony, credential: assertion(credential) });
    });
  }

  const enrol = document.querySelector("form[data-enrol]");
  if (enrol) {
    ceremony(enrol, async (data) => {
      const start = await post("/enroll/start", { code: data.get("code"), name: data.get("name") });
      const credential = await navigator.credentials.create({
        publicKey: creationOptions(start.options.publicKey),
      });
      return post("/enroll/finish", {
        ceremony: start.ceremony,
        credential: registration(credential),
      });
    });
  }

  for (const button of document.querySelectorAll("[data-retry]")) {
    button.addEventListener("click", () => location.reload());
  }

  const offline = document.querySelector("[data-offline]");
  const showOffline = () => {
    if (offline) offline.hidden = navigator.onLine;
  };
  addEventListener("online", () => {
    showOffline();
    if (document.body.dataset.page === "offline") location.reload();
  });
  addEventListener("offline", showOffline);
  showOffline();

  if ("serviceWorker" in navigator) {
    navigator.serviceWorker.register("/sw.js", { scope: "/" }).catch(() => {});
  }
})();
