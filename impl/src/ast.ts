// AST of the reference implementation — the shape the tree-sitter CST
// lowers into (parse.ts) and the semantics consume (engine.ts).

export type TypeAst =
  | { k: 'prim'; name: string }
  | { k: 'lit'; v: any }
  | { k: 'range'; lo: any; hi: any; excl: boolean }
  | { k: 'pattern'; re: string }
  | { k: 'record'; members: MemberAst[]; open: boolean }
  | { k: 'map'; key: TypeAst; val: TypeAst }
  | { k: 'array'; elem: TypeAst; lo?: number; hi?: number }
  | { k: 'union'; arms: TypeAst[] }
  | { k: 'isect'; arms: TypeAst[] }
  | { k: 'func'; params: TypeAst[]; ret: TypeAst }
  | { k: 'named'; name: string; args: TypeAst[]; preds?: Expr[]; ext?: TypeAst };

export type MemberAst =
  | { m: 'value'; name: string; opt: boolean; type: TypeAst; dflt?: Expr }
  | { m: 'derived'; name: string; type?: TypeAst; expr: Expr }
  | { m: 'context'; variable: string; type: TypeAst }
  | { m: 'assert'; name: string; cond: Expr; tail?: ElseTail }
  | { m: 'when'; cond: Expr; body: MemberAst[] };

export type TemplateParts = (string | Expr)[];

export type ElseTail =
  | { t: 'inline'; severity: string; template: TemplateParts }
  | { t: 'ref'; name: string; args: Expr[] };

export type Expr =
  | { e: 'lit'; v: any }
  | { e: 'unitlit'; num: number; unit: string }
  | { e: 'template'; parts: TemplateParts }
  | { e: 'name'; name: string }
  | { e: 'ctx'; name: string }
  | { e: 'referrers'; type: string; member: string }
  | { e: 'obj'; entries: { key: string; val: Expr }[] }
  | { e: 'arr'; items: { spread: boolean; expr: Expr }[] }
  | { e: 'comp'; head: Expr; clauses: { v: string; iter: Expr; filters: Expr[] }[] }
  | { e: 'mapcomp'; key: Expr; val: Expr; clauses: { v: string; iter: Expr; filters: Expr[] }[] }
  | { e: 'bin'; op: string; l: Expr; r: Expr }
  | { e: 'un'; op: string; x: Expr }
  | { e: 'paren'; x: Expr }
  | { e: 'if'; c: Expr; t: Expr; f: Expr }
  | { e: 'lambda'; params: string[]; body: Expr }
  | { e: 'call'; fn: Expr; args: Expr[] }
  | { e: 'member'; x: Expr; name: string; safe?: boolean }
  | { e: 'index'; x: Expr; i: Expr }
  | { e: 'with'; base: Expr; patch: Expr }
  | { e: 'match'; subject: Expr; arms: { v: string; type?: TypeAst; body: Expr }[] };

export type Decl =
  | { d: 'type'; name: string; params?: { name: string; type?: TypeAst }[]; type: TypeAst; tail?: ElseTail }
  | { d: 'const'; name: string; type?: TypeAst; expr: Expr }
  | { d: 'func'; name: string; params: { name: string; type: TypeAst }[]; ret?: TypeAst; body: Expr }
  | { d: 'output'; name: string; type: TypeAst; expr: Expr }
  | { d: 'input'; name: string; type: TypeAst; fallback?: Expr }
  | { d: 'diagnostic'; name: string; params: { name: string; type: TypeAst }[]; severity: string; template: TemplateParts }
  | { d: 'dimension'; name: string }
  | { d: 'unit'; name: string }
  | { d: 'import' }
  | { d: 're_export' };
