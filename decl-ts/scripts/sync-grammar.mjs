// Keep the native packages' generated grammar copies exact without
// invalidating their compiled extensions when the source bytes match.
import { copyFileSync, lstatSync, mkdirSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';

function prepareDirectory(directory, names) {
  const stat = lstatSync(directory, { throwIfNoEntry: false });
  if (stat && !stat.isDirectory()) rmSync(directory, { recursive: true, force: true });
  mkdirSync(directory, { recursive: true });
  const expected = new Set(names);
  for (const name of readdirSync(directory)) {
    if (!expected.has(name)) rmSync(join(directory, name), { recursive: true, force: true });
  }
}

function copyIfChanged(source, destination) {
  const stat = lstatSync(destination, { throwIfNoEntry: false });
  const content = readFileSync(source);
  if (stat?.isFile() && stat.size === content.length && readFileSync(destination).equals(content))
    return;
  // Replace stale directories or links as the previous clean-copy build did.
  if (stat) rmSync(destination, { recursive: true, force: true });
  copyFileSync(source, destination);
}

export function syncGrammar(source, destination) {
  const sources = ['parser.c', 'scanner.c'];
  const headers = readdirSync(join(source, 'tree_sitter'));
  prepareDirectory(destination, [...sources, 'tree_sitter']);
  prepareDirectory(join(destination, 'tree_sitter'), headers);
  for (const name of sources) copyIfChanged(join(source, name), join(destination, name));
  for (const name of headers)
    copyIfChanged(join(source, 'tree_sitter', name), join(destination, 'tree_sitter', name));
}
