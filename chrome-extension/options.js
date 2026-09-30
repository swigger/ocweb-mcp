const connectionStatus = document.getElementById("connectionStatus");
const accessMode = document.getElementById("accessMode");
const pairingString = document.getElementById("pairingString");
const pair = document.getElementById("pair");
const disconnect = document.getElementById("disconnect");
const message = document.getElementById("message");
const retiredCustody = document.getElementById("retiredCustody");

async function refresh() {
  const status = await chrome.runtime.sendMessage({ type: "getStatus" });
  const custodyBlocked = status.retiredCopilotCustodyBlocked === true;
  retiredCustody.classList.toggle("hidden", !custodyBlocked);
  connectionStatus.textContent = status.paired
    ? custodyBlocked
      ? "Paired; automation paused"
      : status.state === "on"
        ? "Connected"
        : "Paired; relay unavailable"
    : "Not paired — connect to the Rust service below";
  accessMode.value = status.accessMode === "selected" ? "selected" : "all";
  accessMode.disabled = !status.paired || custodyBlocked;
  pairingString.disabled = custodyBlocked;
  pair.disabled = custodyBlocked;
  disconnect.disabled = !status.paired && !custodyBlocked;
}

async function showResult(task, success) {
  try {
    const result = await task();
    if (result?.ok === false) {
      throw new Error(result.error ?? "Operation failed.");
    }
    message.textContent = success;
  } catch (error) {
    message.textContent = error instanceof Error ? error.message : String(error);
  }
  await refresh();
}

accessMode.addEventListener("change", () => {
  void showResult(
    () => chrome.runtime.sendMessage({ type: "setAccessMode", accessMode: accessMode.value }),
    "Access mode updated.",
  );
});
pair.addEventListener("click", () => {
  void showResult(
    async () => {
      const result = await chrome.runtime.sendMessage({
        type: "pair",
        pairingString: pairingString.value,
        accessMode: accessMode.value,
      });
      if (result?.ok) pairingString.value = "";
      return result;
    },
    "Pairing saved. The extension will connect to OCWeb.",
  );
});
disconnect.addEventListener("click", () => {
  void showResult(
    () => chrome.runtime.sendMessage({ type: "unpair" }),
    "Disconnected. Pair again to reconnect.",
  );
});

void refresh();

chrome.runtime.onMessage.addListener((message) => {
  if (message?.type === "relayStatusChanged") void refresh();
});
