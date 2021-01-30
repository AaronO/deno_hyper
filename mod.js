const body = Deno.core.encode("ok");

globalThis.handler = async ({ url }) => {
  return {
    status: 200,
    headers: { "content-type": "text/plain" },
    body,
  };
};
