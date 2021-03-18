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
  // async function main() {
  //   while (true) {
  //     const req = await Deno.core.jsonOpAsync("op_next_request", {});
  //     handle(req);
  //   }

  //   async function handle(req) {
  //     let { requestBodyRid, responseSenderRid, method, headers, url } = req;

  //     if (requestBodyRid) {
  //       Deno.core.close(requestBodyRid);
  //     }

  //     const resp = await globalThis.handler({ method, headers, url });

  //     const body = resp.body ?? new Uint8Array();

  //     const zeroCopyBufs = resp.body instanceof Uint8Array ? [resp.body] : [];
  //     const { responseBodyRid } = Deno.core.jsonOpSync("op_respond", {
  //       rid: responseSenderRid,
  //       status: resp.status ?? 200,
  //       headers: Object.entries(resp.headers ?? {}),
  //     }, ...zeroCopyBufs);

  //     if (responseBodyRid) {
  //       for await (const chunk of body) {
  //         await Deno.core.jsonOpAsync(
  //           "op_response_write",
  //           { rid: responseBodyRid },
  //           chunk,
  //         );
  //       }
  //       Deno.core.close(responseBodyRid);
  //     }
  //   }
  // }

  // main();
})(globalThis);
