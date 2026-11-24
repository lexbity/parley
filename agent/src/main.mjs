import { readFile, readlink } from "node:fs/promises"
import { stdin, stdout } from "node:process"
import { createRelay } from "./relay.mjs"
import { executeStructured } from "./structured.mjs"
import {
  compileSchema, MAX_FRAME, parseJson, readFrames, sha256, WorkerFailure, writeFrame,
} from "./protocol.mjs"

async function main() {
  const runtimeSchema = await readFile(new URL("./runtime-v1.schema.json", import.meta.url))
  const coachSchema = await readFile(new URL("./coach.schema.json", import.meta.url))
  const builderSchema = await readFile(new URL("./template_builder.schema.json", import.meta.url))
  const interviewerSchema = await readFile(new URL("./interviewer.schema.json", import.meta.url))
  const brieferSchema = await readFile(new URL("./briefer.schema.json", import.meta.url))
  const build = (await readFile(new URL("./worker-build", import.meta.url), "ascii")).trim()
  const validate = compileSchema(parseJson(runtimeSchema))
  const namespaces = {}
  for (const namespace of ["user", "net", "pid", "ipc", "uts"]) {
    namespaces[namespace] = await readlink("/proc/self/ns/" + namespace)
  }
  await writeFrame(stdout, validate, {
    v: 1, kind: "hello", seq: 1,
    body: { build, schema_digest: sha256(runtimeSchema), capabilities: ["structured_role"], namespaces },
  })

  const frames = readFrames(stdin, validate)
  const ack = (await frames.next()).value
  if (ack?.kind !== "hello_ack" || ack.body.max_frame_bytes !== MAX_FRAME) {
    throw new WorkerFailure("WORKER_PROTOCOL")
  }
  const execution = (await frames.next()).value
  if (execution?.kind !== "execute") throw new WorkerFailure("WORKER_PROTOCOL")
  const ids = { execution_id: execution.execution_id, lease_id: execution.lease_id }
  const abort = new AbortController()
  const deadline = AbortSignal.timeout(execution.body.deadline_ms)
  const signal = AbortSignal.any([abort.signal, deadline])
  const relay = createRelay("/relay/provider.sock", signal)
  const readControl = frames.next().then(({ value }) => {
    if (value?.kind === "cancel" && value.execution_id === ids.execution_id && value.lease_id === ids.lease_id) {
      throw new WorkerFailure("CANCELLED")
    }
    throw new WorkerFailure("WORKER_PROTOCOL")
  })
  try {
    const tokenizer = await readFile("/tokenizer/artifact.json")
    const body = await Promise.race([
      readControl,
      executeStructured(execution.body, { tokenizer, coachSchema, builderSchema, interviewerSchema, brieferSchema }, relay.fetch, signal),
    ])
    await writeFrame(stdout, validate, { v: 1, kind: "complete", seq: 2, ...ids, body })
  } catch (error) {
    const code = error instanceof WorkerFailure ? error.code : "PROVIDER_FAILED"
    await writeFrame(stdout, validate, { v: 1, kind: "failed", seq: 2, ...ids, body: { code } })
  } finally {
    abort.abort()
    await relay.close()
    stdin.destroy()
  }
}

main().catch(() => {
  process.exitCode = 1
  stdin.destroy()
})
