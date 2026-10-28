// A worker of the threaded wasm build: a second instance of the game's module on the page's shared
// memory, which does nothing but run the jobs the engine queues (`src/web.rs`, `thread_entry`,
// which does not return).

onmessage = async (e) => {
  const { module, memory } = e.data;
  const { default: init, thread_entry } = await import("./benilla-lambo.js");
  await init({ module_or_path: module, memory });
  thread_entry();
};
