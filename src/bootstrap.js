((window) => {
  Deno.core.ops();
  Deno.core.registerErrorClass("Error", Error);

  Object.assign(window, {
    window: globalThis,
  });

  async function handleConn(connRid) {
    while (true) {
      const request = await Deno.core.jsonOpAsync("op_next_request", {
        rid: connRid,
      });
      // Deno.core.print(`request ${JSON.stringify(request)}\n`);
      Deno.core.jsonOpSync("op_respond", {
        rid: connRid,
        status: 200,
        headers: Object.entries({ "x-foo": "bar" }),
      });
    }
  }

  async function main() {
    const { rid: serverRid } = await Deno.core.jsonOpAsync(
      "op_create_server",
      {},
    );
    // Deno.core.print(`server rid ${serverRid}\n`);

    while (true) {
      const { rid: connRid } = await Deno.core.jsonOpAsync("op_accept", {
        rid: serverRid,
      });
      // Deno.core.print(`conn rid ${connRid}\n`);
      handleConn(connRid);
    }
  }

  main();
})(globalThis);
