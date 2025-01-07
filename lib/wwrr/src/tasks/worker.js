Error.stackTraceLimit = 500;
globalThis.onerror = console.error;

const pendingMessages = [];
let worker = undefined;
const handleMessage = async data => {
  if (worker) {
    await worker.handle(data);
  } else {
    // We start off by buffering up all messages until we finish initializing.
    pendingMessages.push(data);
  }
};

globalThis.onmessage = async ev => {
  if (ev.data.type == "init") {
    const { role, memory, module, id, import_url } = ev.data;

    // Import WWRR module.
    let absolute_url;
    if (globalThis.location.origin && globalThis.location.origin != "null")
      absolute_url = new URL(import_url, globalThis.location.origin);
    else
      absolute_url = new URL(import_url);
    const imported = await import(absolute_url);

    // Initialize.
    await imported.default({ module: module, memory: memory });

    // Start worker.
    if (role == "scheduler")
      worker = new imported.SchedulerWorker();
    else if (role == "worker")
      worker = new imported.ThreadPoolWorker(id);
    else
      throw new Error(`unknown role ${role}`);

    // Now that we're initialized, we need to handle any buffered messages
    for (const msg of pendingMessages.splice(0, pendingMessages.length))
      await worker.handle(msg);
  } else {
    // Handle the message like normal.
    await handleMessage(ev.data);
  }
};
