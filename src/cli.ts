#!/usr/bin/env node

import { cwd } from 'node:process';

import { doctor, type Diagnostic, type DoctorReport } from './cli/doctor.js';
import { once, type OnceReport } from './cli/once.js';
import { status } from './cli/status.js';
import { terminalText } from './cli/terminal-text.js';
import { EuphoError, messageOf } from './errors.js';

const VERSION = '0.1.0';

interface ParsedArguments {
  command: string;
  options: Map<string, string | true>;
}

async function main(arguments_: string[]): Promise<number> {
  const parsed = parseArguments(arguments_);
  const json = flag(parsed.options, 'json');

  switch (parsed.command) {
    case 'help':
      assertAllowedOptions(parsed.options, new Set());
      process.stdout.write(`${usage()}\n`);
      return 0;
    case 'version':
      assertAllowedOptions(parsed.options, new Set());
      process.stdout.write(`eupho ${VERSION}\n`);
      return 0;
    case 'doctor': {
      assertAllowedOptions(parsed.options, new Set(['repo', 'config', 'host-config', 'json']));
      const report = await doctor({
        cwd: cwd(),
        ...optionalString(parsed.options, 'repo', 'repository'),
        ...optionalString(parsed.options, 'config', 'configPath'),
        ...optionalString(parsed.options, 'host-config', 'hostConfigPath'),
      });
      writeResult(report, json, renderDoctor);
      return report.ok ? 0 : 1;
    }
    case 'once': {
      assertAllowedOptions(parsed.options, new Set(['repo', 'config', 'host-config', 'json']));
      const report = await once({
        cwd: cwd(),
        repository: requiredString(parsed.options, 'repo'),
        ...optionalString(parsed.options, 'config', 'configPath'),
        ...optionalString(parsed.options, 'host-config', 'hostConfigPath'),
      });
      writeResult(report, json, renderOnce);
      return 0;
    }
    case 'status': {
      assertAllowedOptions(parsed.options, new Set(['state-root', 'host-config', 'json']));
      const report = await status({
        cwd: cwd(),
        ...optionalString(parsed.options, 'state-root', 'stateRoot'),
        ...optionalString(parsed.options, 'host-config', 'hostConfigPath'),
      });
      writeResult(report, json, renderStatus);
      return 0;
    }
    default:
      throw new EuphoError(
        'unknown_command',
        `Unknown command ${parsed.command}. Run eupho help for usage.`,
        2,
      );
  }
}

function parseArguments(arguments_: string[]): ParsedArguments {
  if (arguments_.length === 0) return { command: 'help', options: new Map() };
  const [first, ...rest] = arguments_;
  if (first === undefined) return { command: 'help', options: new Map() };
  if (first === '-h' || first === '--help') return { command: 'help', options: new Map() };
  if (first === '-v' || first === '--version') return { command: 'version', options: new Map() };
  if (first.startsWith('-')) {
    throw new EuphoError('missing_command', `Expected a command before ${first}`, 2);
  }

  const options = new Map<string, string | true>();
  for (let index = 0; index < rest.length; index += 1) {
    const token = rest[index];
    if (token === undefined || !token.startsWith('--')) {
      throw new EuphoError('unexpected_argument', `Unexpected argument ${token ?? '<missing>'}`, 2);
    }
    const name = token.slice(2);
    if (name.length === 0) throw new EuphoError('invalid_option', 'Option name cannot be empty', 2);
    if (options.has(name)) throw new EuphoError('duplicate_option', `Option --${name} was repeated`, 2);
    if (name === 'json') {
      options.set(name, true);
      continue;
    }
    const value = rest[index + 1];
    if (value === undefined || value.startsWith('--')) {
      throw new EuphoError('missing_option_value', `Option --${name} requires a value`, 2);
    }
    options.set(name, value);
    index += 1;
  }
  return { command: first, options };
}

function assertAllowedOptions(options: Map<string, string | true>, allowed: ReadonlySet<string>): void {
  for (const name of options.keys()) {
    if (!allowed.has(name)) {
      throw new EuphoError('unknown_option', `Unknown option --${name}`, 2);
    }
  }
}

function requiredString(options: Map<string, string | true>, name: string): string {
  const value = options.get(name);
  if (typeof value !== 'string' || value.length === 0) {
    throw new EuphoError('missing_required_option', `Option --${name} is required`, 2);
  }
  return value;
}

function optionalString<Name extends string>(
  options: Map<string, string | true>,
  optionName: string,
  propertyName: Name,
): { [Key in Name]?: string } {
  const value = options.get(optionName);
  return typeof value === 'string' ? ({ [propertyName]: value } as { [Key in Name]: string }) : {};
}

function flag(options: Map<string, string | true>, name: string): boolean {
  return options.get(name) === true;
}

function writeResult<T>(value: T, json: boolean, render: (value: T) => string): void {
  process.stdout.write(json ? `${JSON.stringify(value, null, 2)}\n` : `${render(value)}\n`);
}

function renderDoctor(report: DoctorReport): string {
  const lines = ['Eupho doctor', ''];
  for (const check of report.checks) lines.push(renderDiagnostic(check));
  lines.push('', report.ok ? 'Ready for the checked capabilities.' : 'Preflight failed. Resolve the failures above.');
  return lines.join('\n');
}

function renderDiagnostic(check: Diagnostic): string {
  const symbol = { pass: 'PASS', fail: 'FAIL', warn: 'WARN', skip: 'SKIP' }[check.status];
  const remediation = check.remediation === null ? '' : `\n       ${check.remediation}`;
  return `[${symbol}] ${check.code}: ${check.message}${remediation}`;
}

function renderOnce(report: OnceReport): string {
  const lines = [
    'Eupho observe-only pass',
    '',
    `Repository: ${report.repository}`,
    `Base:       ${report.baseSha}`,
    `Policy:     ${report.policySource}${report.trustedBase ? ' (trusted base)' : ' (local override)'}`,
    `Observed:   ${report.observedAt}`,
    '',
  ];
  if (report.candidates.length === 0) {
    lines.push('No eligible issues would be claimed.');
  } else {
    lines.push(`${report.candidates.length} issue(s) would be claimed:`);
    for (const candidate of report.candidates) {
      lines.push(
        `  #${candidate.issueNumber} ${terminalText(candidate.issueTitle)}`,
        `    ${candidate.executionMode} / ${candidate.workspaceType} / ${candidate.mergePolicy}`,
      );
    }
  }
  if (report.diagnostics.length > 0) {
    lines.push('', 'Diagnostics:');
    for (const diagnostic of report.diagnostics) {
      lines.push(`  #${diagnostic.issueNumber} ${diagnostic.code}: ${diagnostic.message}`);
    }
  }
  lines.push('', 'No GitHub state was changed.');
  return lines.join('\n');
}

function renderStatus(report: Awaited<ReturnType<typeof status>>): string {
  const lines = ['Eupho local status', '', `State root: ${report.stateRoot}`, ''];
  if (report.repositories.length === 0) {
    lines.push('No observed repository snapshots.');
    return lines.join('\n');
  }
  for (const repository of report.repositories) {
    lines.push(
      `${repository.repository} @ ${repository.baseSha}`,
      `  observed ${repository.observedAt}; ${repository.candidates.length} candidate(s), ${repository.diagnostics.length} diagnostic(s)`,
    );
  }
  return lines.join('\n');
}

function usage(): string {
  return `Eupho ${VERSION} — GitHub-native control plane for coding agents

Usage:
  eupho doctor [--repo OWNER/REPO] [--config PATH] [--host-config PATH] [--json]
  eupho once --repo OWNER/REPO [--config PATH] [--host-config PATH] [--json]
  eupho status [--state-root PATH] [--host-config PATH] [--json]
  eupho help
  eupho version

Phase 1 is observe-only. It never changes GitHub state.`;
}

const jsonRequested = process.argv.includes('--json');
main(process.argv.slice(2))
  .then((exitCode) => {
    process.exitCode = exitCode;
  })
  .catch((error: unknown) => {
    const code = error instanceof EuphoError ? error.code : 'unexpected_error';
    const exitCode = error instanceof EuphoError ? error.exitCode : 1;
    if (jsonRequested) {
      process.stderr.write(`${JSON.stringify({ ok: false, error: { code, message: messageOf(error) } }, null, 2)}\n`);
    } else {
      process.stderr.write(`eupho: ${messageOf(error)}\n`);
    }
    process.exitCode = exitCode;
  });
