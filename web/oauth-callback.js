(() => {
  "use strict";
  const body = document.body;
  const payload = {
    type: "polygate:oauth",
    success: body.dataset.success === "true",
    flowId: body.dataset.flowId || null,
    accountId: body.dataset.accountId || null,
  };
  try {
    if (window.opener && !window.opener.closed) {
      window.opener.postMessage(payload, window.location.origin);
    }
  } catch (_) {
    // The opener may have been closed or navigated away.
  }
  const close = document.getElementById("oauth-close");
  if (close) close.addEventListener("click", () => window.close());
  if (payload.success) {
    setTimeout(() => window.close(), 900);
  }
})();
