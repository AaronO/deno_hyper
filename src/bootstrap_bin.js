((window) => {
  Deno.core.ops();
  Deno.core.registerErrorClass("Error", Error);

  Object.assign(window, {
    window: globalThis,
  });

  async function handleConn(connRid) {
    while (true) {
      await Deno.core.binOpAsync("op_next_request", connRid);
      Deno.core.binOpSync("op_respond", connRid);
    }
  }

  async function main() {
    const serverRid = await Deno.core.binOpAsync(
      "op_create_server",
    );

    while (true) {
      const connRid = await Deno.core.binOpAsync("op_accept", serverRid);
      handleConn(connRid);
    }
  }

  main();
})(globalThis);
