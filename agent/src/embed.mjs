import { Tokenizer } from "@huggingface/tokenizers"
import { RELAY_ORIGIN } from "./relay.mjs"
import { observe } from "./structured.mjs"
import {
  MAX_EMBED_DIMS, MAX_EMBED_INPUT, MAX_EMBED_VECTORS,
  parseJson, sha256, WorkerFailure,
} from "./protocol.mjs"

function reject(code) {
  throw new WorkerFailure(code)
}

function qualifiedTokenizer(bytes, digest) {
  if (sha256(bytes) !== digest) reject("PROFILE_CHANGED")
  try {
    const artifact = parseJson(bytes)
    return new Tokenizer(artifact.tokenizer, artifact.config)
  } catch {
    reject("CONTEXT_BUDGET")
  }
}

function embedIdentity(profile) {
  if (!profile.embed_tag) reject("PROFILE_CHANGED")
  return { providerVersion: profile.provider_version, tag: profile.embed_tag, digest: profile.embed_digest ?? null }
}

/**
 * One bounded embedding batch. The host declares exact dimensions and the
 * provider response must match them in order; any non-finite or mis-sized
 * vector fails the whole execution.
 */
export async function executeEmbed(execution, assets, fetch, signal) {
  const { profile, mode, inputs, dimensions } = execution
  const probe = mode === "probe"
  const tokenizer = qualifiedTokenizer(assets.tokenizer, profile.tokenizer_digest)
  await observe(fetch, embedIdentity(profile))

  const batch = probe ? ["parley-embed-probe"] : inputs
  if (batch.length < 1 || batch.length > MAX_EMBED_VECTORS) reject("WORKER_PROTOCOL")
  if (!probe) {
    if (!Number.isInteger(dimensions) || dimensions < 1 || dimensions > MAX_EMBED_DIMS) {
      reject("WORKER_PROTOCOL")
    }
  }
  const totalBytes = batch.reduce((sum, text) => sum + Buffer.byteLength(text), 0)
  if (totalBytes > MAX_EMBED_INPUT) reject("CONTEXT_BUDGET")

  const tokenCount = batch.reduce((sum, text) => sum + tokenizer.encode(text, { add_special_tokens: false }).ids.length, 0)
  if (tokenCount < 1 || tokenCount > profile.context_tokens) reject("CONTEXT_BUDGET")

  const response = await fetch(RELAY_ORIGIN + "/v1/embeddings", {
    method: "POST",
    body: JSON.stringify({ model: profile.embed_tag, input: batch }),
    signal,
  })
  if (!response.ok) reject("PROVIDER_FAILED")
  const payload = await response.json()
  const data = payload?.data
  if (!Array.isArray(data) || data.length !== batch.length) reject("RESULT_INVALID")
  const vectors = []
  for (const item of data) {
    const vector = item?.embedding
    if (!Array.isArray(vector) || vector.length < 1) reject("RESULT_INVALID")
    if (probe) {
      if (vector.length > MAX_EMBED_DIMS) reject("RESULT_INVALID")
    } else if (vector.length !== dimensions) {
      reject("RESULT_INVALID")
    }
    for (const value of vector) {
      if (typeof value !== "number" || !Number.isFinite(value)) reject("RESULT_INVALID")
    }
    vectors.push(vector)
  }
  if (signal.aborted) reject("CANCELLED")

  const usage = { input_tokens: tokenCount, output_tokens: 0 }
  const observed = await observe(fetch, embedIdentity(profile))
  if (probe) {
    return { mode, observed, usage, dimensions: vectors[0].length }
  }
  return { mode, observed, usage, vectors }
}
