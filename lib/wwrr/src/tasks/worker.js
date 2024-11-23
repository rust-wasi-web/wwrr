Error.stackTraceLimit = 50;
globalThis.onerror = console.error;

let pendingMessages = [];
let worker = undefined;
let handleMessage = async data => {
  if (worker) {
    await worker.handle(data);
  } else {
    // We start off by buffering up all messages until we finish initializing.
    pendingMessages.push(data);
  }
};

globalThis.onmessage = async ev => {
  if (ev.data.type == "init") {
    const { memory, module, id, import_url } = ev.data;
    const imported = await import(new URL(import_url, self.location.origin));

    // Initialize.
    await imported.default({ module: module, memory: memory });

    // Now that we're initialized, we need to handle any buffered messages
    worker = new imported.ThreadPoolWorker(id);
    for (const msg of pendingMessages.splice(0, pendingMessages.length)) {
      await worker.handle(msg);
    }
  } else {
    // Handle the message like normal.
    await handleMessage(ev.data);
  }
};
