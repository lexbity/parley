import { createHash } from 'node:crypto';
import type { PartyImportMapping, PartyImportPreview } from '../src/api/client';
export const maxImportBytes = 1024 * 1024;
export const maxImportRows = 1000;
export type CsvRecord = { number: number; cells: string[] };
export function parsePartyCsv(bytes: Buffer) {
  if (bytes.length > maxImportBytes) throw new Error('Split this CSV into files of at most ' + maxImportBytes + ' bytes.');
  const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  const records: CsvRecord[] = [];
  let recordNumber = 1;
  let cells: string[] = [], field = '', quoted = false, endedQuote = false;
  function endField() { cells.push(field); field = ''; endedQuote = false; }
  function endRecord() {
    endField();
    if (cells.some(c => c.trim() !== '')) records.push({ number: recordNumber, cells });
    cells = []; recordNumber += 1;
    if (records.length > maxImportRows + 1) throw new Error('Split the CSV into files of at most ' + maxImportRows + ' data rows.');
  }
  for (let i = 0; i < text.length; i++) {
    const ch = text[i]!;
    if (quoted) {
      if (ch !== '"') field += ch;
      else if (text[i + 1] === '"') { field += '"'; i++; }
      else { quoted = false; endedQuote = true; }
    } else if (ch === ',') endField();
    else if (ch === '\n' || ch === '\r') {
      if (ch === '\r' && text[i + 1] === '\n') i++;
      endRecord();
    } else if (ch === '"' && !endedQuote && !field) quoted = true;
    else if (endedQuote || ch === '"') throw new Error('Malformed quoting in record ' + recordNumber + '.');
    else field += ch;
  }
  if (quoted) throw new Error('Unclosed quote in record ' + recordNumber + '.');
  endRecord();
  const header = records.shift();
  if (!header) throw new Error('Include one header row.');
  for (const row of records) if (row.cells.length !== header.cells.length) throw new Error('Wrong number of fields in record ' + row.number + '.');
  return { headers: header.cells, records };
}

type Party = { id: string; name: string; kind: string; focus: string };
export function previewCsv(csv: ReturnType<typeof parsePartyCsv>, mapping: PartyImportMapping, parties: Iterable<Party>) {
  const columns = [mapping.name_column, mapping.focus_column, mapping.notes_column].filter((v): v is number => v !== null && v !== undefined);
  const numbers = new Set(csv.records.map(row => row.number));
  if (!['person', 'org'].includes(mapping.party_type) || !Number.isInteger(mapping.name_column) ||
      columns.some(c => !Number.isInteger(c) || c < 0 || c >= csv.headers.length) || new Set(columns).size !== columns.length) throw new Error('Choose a different column for each field.');
  if (!Array.isArray(mapping.excluded_records) || !Array.isArray(mapping.distinct_records) ||
      [...mapping.excluded_records,...mapping.distinct_records].some(index => !numbers.has(index))) throw new Error('Unknown source record.');
  const candidates = new Map<string,string[]>();
  function add(name: string, value: string) {
    const key = name.trim().toLowerCase();
    if (!key) return;
    const matches = candidates.get(key) ?? [];
    matches.push(value); candidates.set(key, matches);
  }
  for (const party of parties) {
    if (['person','org'].includes(party.kind)) add(party.name, [party.name, party.kind, party.focus, party.id].filter(Boolean).join(' · '));
  }
  for (const row of csv.records) add(row.cells[mapping.name_column]!, 'Record ' + row.number);
  const rows = csv.records.map((row): PartyImportPreview['rows'][number] => {
    const name = row.cells[mapping.name_column]!.trim();
    const focus = mapping.focus_column == null ? '' : row.cells[mapping.focus_column]!;
    const notes = mapping.notes_column == null ? '' : row.cells[mapping.notes_column]!.replace(/\r\n?/g, '\n');
    const duplicates = (candidates.get(name.toLowerCase()) ?? []).filter(v => v !== 'Record ' + row.number);
    const decision = mapping.excluded_records.includes(row.number) ? 'skip' :
      !name || name.length > 500 || focus.length > 250 ? 'error' :
        duplicates.length && !mapping.distinct_records.includes(row.number) ? 'review' : 'create';
    return { source_record: row.number, name, focus, notes, duplicates, decision };
  });
  return { rows, create_count: rows.filter(r => r.decision === 'create').length, skip_count: rows.filter(r => r.decision === 'skip').length, error_count: rows.filter(r => r.decision === 'error' || r.decision === 'review').length };
}
export function importDigest(value: unknown) { return createHash('sha256').update(JSON.stringify(value)).digest('hex'); }
