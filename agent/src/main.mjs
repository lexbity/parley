import { readFile, readlink } from "node:fs/promises"
import { stdin, stdout } from "node:process"
import { createRelay } from "./relay.mjs"
import { executeStructured } from "./structured.mjs"
import { executeDelegated } from "./delegated.mjs"
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
    body: {
      build,
      schema_digest: sha256(runtimeSchema),
      capabilities: ["structured_role", "delegated"],
      namespaces,
    },
  })

  const frames = readFrames(stdin, validate)
  const ack = (await frames.next()).value
  if (ack?.kind !== "hello_ack" || ack.body.max_frame_bytes !== MAX_FRAME ||
      ack.body.capabilities?.join(",") !== "structured_role,delegated") {
    throw new WorkerFailure("WORKER_PROTOCOL")
  }
  const execution = (await frames.next()).value
  if (execution?.kind !== "execute") throw new WorkerFailure("WORKER_PROTOCOL")
  const ids = { execution_id: execution.execution_id, lease_id: execution.lease_id }
  const abort = new AbortController()
  const deadline = AbortSignal.timeout(execution.body.deadline_ms)
  const signal = AbortSignal.any([abort.signal, deadline])
  const delegated = execution.body.mode === "delegated"
  const relay = createRelay("/relay/provider.sock", signal, { chatCalls: delegated ? 16 : 1 })
  const readControl = delegated ? null : frames.next().then(({ value }) => {
    if (value?.kind === "cancel" && value.execution_id === ids.execution_id && value.lease_id === ids.lease_id) {
      throw new WorkerFailure("CANCELLED")
    }
    throw new WorkerFailure("WORKER_PROTOCOL")
  })
  let workerSeq = 3
  const assets = { tokenizer: null, coachSchema, builderSchema, interviewerSchema, brieferSchema }
  const control = {
    async next() {
      const { value } = await frames.next()
      if (!value) throw new WorkerFailure("WORKER_PROTOCOL")
      if (value.kind === "cancel" && value.execution_id === ids.execution_id && value.lease_id === ids.lease_id) {
        throw new WorkerFailure("CANCELLED")
      }
      if (value.execution_id !== ids.execution_id || value.lease_id !== ids.lease_id) {
        throw new WorkerFailure("WORKER_PROTOCOL")
      }
      if (!["turn_accepted", "tool_result", "suspend"].includes(value.kind)) {
        throw new WorkerFailure("WORKER_PROTOCOL")
      }
      return value
    },
    async send(kind, body) {
      await writeFrame(stdout, validate, { v: 1, kind, seq: workerSeq++, ...ids, body })
    },
  }
  try {
    let result
    if (delegated) {
      assets.tokenizer = await readFile("/tokenizer/artifact.json")
      result = await executeDelegated(execution.body, assets, relay.fetch, signal, control)
      if (result.suspended) {
        // A durable wait ends the lease. The host reaps this process and
        // later starts a fresh execution with canonical reconstructed history.
        stdin.destroy()
        return
      }
      result = result.body
    } else {
      assets.tokenizer = await readFile("/tokenizer/artifact.json")
      result = await Promise.race([
        readControl,
        executeStructured(execution.body, assets, relay.fetch, signal),
      ])
    }
    const terminalSeq = delegated ? workerSeq : 2
    await writeFrame(stdout, validate, { v: 1, kind: "complete", seq: terminalSeq, ...ids, body: result })
  } catch (error) {
    const code = error instanceof WorkerFailure ? error.code : "PROVIDER_FAILED"
    const terminalSeq = delegated ? workerSeq : 2
    await writeFrame(stdout, validate, { v: 1, kind: "failed", seq: terminalSeq, ...ids, body: { code } })
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
