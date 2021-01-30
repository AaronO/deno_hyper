((window) => {
  Deno.core.ops();

  Object.assign(window, {
    window: globalThis,
  });

  async function main() {
    while (true) {
      const req = await Deno.core.jsonOpAsync("op_next_request", {});
      handle(req);
    }

    async function handle(req) {
      let { requestBodyRid, responseSenderRid, method, headers, url } = req;

      Deno.core.close(requestBodyRid);

      const resp = await globalThis.handler({ method, headers, url });

      const body = resp.body ?? [];

      const { responseBodyRid } = Deno.core.jsonOpSync("op_respond", {
        rid: responseSenderRid,
        status: resp.status ?? 200,
        headers: Object.entries(resp.headers ?? {}),
      });

      for await (const chunk of body) {
        await Deno.core.jsonOpAsync(
          "op_response_write",
          { rid: responseBodyRid },
          chunk,
        );
      }

      Deno.core.close(responseBodyRid);
    }
  }

  main();
})(globalThis);
