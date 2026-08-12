export class EuphoError extends Error {
  readonly code: string;
  readonly exitCode: number;

  constructor(code: string, message: string, exitCode = 1, options?: ErrorOptions) {
    super(message, options);
    this.name = 'EuphoError';
    this.code = code;
    this.exitCode = exitCode;
  }
}

export function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
