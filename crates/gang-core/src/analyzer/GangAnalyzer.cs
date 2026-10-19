// GANG's C# analyzer (DESIGN.md §14.2): finds every place where code gives
// Unity a layer number, by following values the way the compiler does.
//
// It is compiled on first use with the Roslyn compiler that ships inside the
// Unity editor and run with the editor's own .NET runtime, so nothing is
// downloaded or installed. Input: a JSON request (source files, reference
// assemblies, preprocessor symbols). Output on stdout: JSON with every layer
// index or layer mask that reaches Unity, traced back to the literal that
// produced it, and every place where the value could not be traced.

using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Operations;

namespace Gang
{
    enum Role { Index, Mask }

    sealed class Use
    {
        public string File = "";
        public int Line;
        public int Column;
        public string Text = "";
        public string Role = "";
        public long? Value;
        public string Via = "";
        // literal: the number written in the source, safe to rewrite.
        // implicit-enum: an enum member without a value.
        // computed: a constant expression (A + 2); not rewritten.
        // external: a constant from a compiled library.
        // unknown: a value that is not a constant (read at run time).
        // mixed: a variable used both as a layer and as a mask.
        public string Kind = "";
    }

    sealed class Field
    {
        public string Type = "";
        public string Name = "";
        public string Role = "";
    }

    static class Program
    {
        static int Main(string[] args)
        {
            try
            {
                var request = JsonDocument.Parse(File.ReadAllText(args[0])).RootElement;
                var files = Strings(request, "files");
                var references = Strings(request, "references");
                var defines = Strings(request, "defines");
                var analysis = new Analysis(files, references, defines);
                analysis.Run();
                var options = new JsonSerializerOptions { IncludeFields = true, PropertyNamingPolicy = JsonNamingPolicy.CamelCase };
                Console.Out.Write(JsonSerializer.Serialize(new Output
                {
                    files = files.Count,
                    errors = analysis.Errors,
                    messages = analysis.Messages,
                    uses = analysis.Uses,
                    fields = analysis.Fields,
                }, options));
                return 0;
            }
            catch (Exception e)
            {
                Console.Error.WriteLine(e.ToString());
                return 2;
            }
        }

        static List<string> Strings(JsonElement root, string name)
        {
            var list = new List<string>();
            if (root.TryGetProperty(name, out var items))
                foreach (var item in items.EnumerateArray())
                    list.Add(item.GetString() ?? "");
            return list;
        }
    }

    sealed class Output
    {
        public int files;
        public int errors;
        public List<string> messages = new();
        public List<Use> uses = new();
        public List<Field> fields = new();
    }

    sealed class Analysis
    {
        readonly CSharpCompilation compilation;
        readonly Dictionary<SyntaxTree, SemanticModel> models = new();
        readonly Dictionary<ISymbol, List<IOperation>> writes = new(SymbolEqualityComparer.Default);
        readonly Dictionary<ISymbol, List<IOperation>> arguments = new(SymbolEqualityComparer.Default);
        readonly Dictionary<ISymbol, List<IOperation>> returns = new(SymbolEqualityComparer.Default);
        readonly Dictionary<ISymbol, Role> carriers = new(SymbolEqualityComparer.Default);
        readonly Dictionary<ISymbol, string> carrierVia = new(SymbolEqualityComparer.Default);
        readonly HashSet<ISymbol> mixed = new(SymbolEqualityComparer.Default);
        readonly Queue<(IOperation op, Role role, string via)> work = new();
        readonly HashSet<string> seen = new();
        readonly HashSet<(IOperation, Role)> traced = new();
        public readonly List<Use> Uses = new();
        public readonly List<Field> Fields = new();
        public int Errors;
        public readonly List<string> Messages = new();

        public Analysis(List<string> files, List<string> references, List<string> defines)
        {
            var parse = new CSharpParseOptions(LanguageVersion.Latest, preprocessorSymbols: defines);
            var trees = files
                .Where(File.Exists)
                .Select(f => CSharpSyntaxTree.ParseText(File.ReadAllText(f), parse, path: f))
                .ToList();
            var refs = references
                .Where(File.Exists)
                .Select(r => (MetadataReference)MetadataReference.CreateFromFile(r))
                .ToList();
            compilation = CSharpCompilation.Create(
                "GangAnalysis",
                trees,
                refs,
                new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, allowUnsafe: true));
        }

        SemanticModel Model(SyntaxTree tree)
        {
            if (!models.TryGetValue(tree, out var model))
            {
                model = compilation.GetSemanticModel(tree);
                models[tree] = model;
            }
            return model;
        }

        public void Run()
        {
            var errors = compilation.GetDiagnostics().Where(d => d.Severity == DiagnosticSeverity.Error).ToList();
            Errors = errors.Count;
            Messages.AddRange(errors.Take(10).Select(d => d.ToString()));
            foreach (var tree in compilation.SyntaxTrees)
            {
                var model = Model(tree);
                foreach (var root in Roots(tree, model))
                    foreach (var op in root.DescendantsAndSelf())
                        Index(op, model);
            }
            while (work.Count > 0)
            {
                var (op, role, via) = work.Dequeue();
                Trace(op, role, via);
            }
            foreach (var pair in carriers)
            {
                if (pair.Key is IFieldSymbol field && Serialized(field))
                {
                    Fields.Add(new Field
                    {
                        Type = field.ContainingType.ToDisplayString(),
                        Name = field.Name,
                        Role = mixed.Contains(field) ? "mixed" : Name(pair.Value),
                    });
                }
            }
        }

        static string Name(Role role) => role == Role.Index ? "index" : "mask";

        /// Operations of every member body and initializer.
        static IEnumerable<IOperation> Roots(SyntaxTree tree, SemanticModel model)
        {
            foreach (var node in tree.GetRoot().DescendantNodes())
            {
                IOperation? op = node switch
                {
                    BaseMethodDeclarationSyntax m => model.GetOperation(m),
                    AccessorDeclarationSyntax a => model.GetOperation(a),
                    ArrowExpressionClauseSyntax e when e.Parent is PropertyDeclarationSyntax or IndexerDeclarationSyntax => model.GetOperation(e),
                    EqualsValueClauseSyntax v when v.Parent is VariableDeclaratorSyntax { Parent.Parent: FieldDeclarationSyntax } => model.GetOperation(v),
                    EqualsValueClauseSyntax v when v.Parent is PropertyDeclarationSyntax => model.GetOperation(v),
                    GlobalStatementSyntax g => model.GetOperation(g.Statement),
                    _ => null,
                };
                if (op != null)
                    yield return op;
            }
        }

        static IOperation Unwrap(IOperation op)
        {
            while (true)
            {
                switch (op)
                {
                    case IConversionOperation c when c.Operand != null:
                        op = c.Operand;
                        continue;
                    case IParenthesizedOperation p:
                        op = p.Operand;
                        continue;
                    default:
                        return op;
                }
            }
        }

        static void Add(Dictionary<ISymbol, List<IOperation>> map, ISymbol? key, IOperation? value)
        {
            if (key == null || value == null)
                return;
            if (!map.TryGetValue(key, out var list))
                map[key] = list = new List<IOperation>();
            list.Add(value);
        }

        static ISymbol? Target(IOperation op) => Unwrap(op) switch
        {
            ILocalReferenceOperation l => l.Local,
            IFieldReferenceOperation f => f.Field,
            IPropertyReferenceOperation p => p.Property,
            IParameterReferenceOperation p => p.Parameter,
            _ => null,
        };

        void Index(IOperation op, SemanticModel model)
        {
            switch (op)
            {
                case ISimpleAssignmentOperation a:
                    Add(writes, Target(a.Target), a.Value);
                    Sink(MemberRole(Target(a.Target)), a.Value, Describe(a.Target));
                    break;
                case ICompoundAssignmentOperation c:
                    Add(writes, Target(c.Target), c.Value);
                    Sink(MemberRole(Target(c.Target)), c.Value, Describe(c.Target));
                    break;
                case IVariableDeclaratorOperation d when d.Initializer != null:
                    Add(writes, d.Symbol, d.Initializer.Value);
                    break;
                case IFieldInitializerOperation f:
                    foreach (var field in f.InitializedFields)
                        Add(writes, field, f.Value);
                    break;
                case IPropertyInitializerOperation p:
                    foreach (var property in p.InitializedProperties)
                        Add(writes, property, p.Value);
                    break;
                case IReturnOperation r when r.ReturnedValue != null:
                    {
                        var method = model.GetEnclosingSymbol(r.Syntax.SpanStart);
                        if (method is IMethodSymbol m && m.MethodKind == MethodKind.PropertyGet && m.AssociatedSymbol != null)
                            Add(returns, m.AssociatedSymbol, r.ReturnedValue);
                        Add(returns, method, r.ReturnedValue);
                        break;
                    }
                case IArgumentOperation arg when arg.Parameter != null && arg.ArgumentKind != ArgumentKind.DefaultValue:
                    Add(arguments, arg.Parameter.OriginalDefinition, arg.Value);
                    if (arg.Parameter.ContainingSymbol is IMethodSymbol callee)
                        Sink(ParameterRole(arg.Parameter), arg.Value, $"{callee.ContainingType?.Name}.{callee.Name}({arg.Parameter.Name})");
                    break;
                case IConversionOperation conv when IsLayerMask(conv.Type) && conv.Operand.Type != null && IsIntegral(conv.Operand.Type):
                    Sink(Role.Mask, conv.Operand, "LayerMask");
                    break;
                case IBinaryOperation b when b.OperatorKind is BinaryOperatorKind.Equals or BinaryOperatorKind.NotEquals:
                    if (IsLayerProperty(b.LeftOperand))
                        Sink(Role.Index, b.RightOperand, "GameObject.layer ==");
                    else if (IsLayerProperty(b.RightOperand))
                        Sink(Role.Index, b.LeftOperand, "GameObject.layer ==");
                    break;
                case ISwitchOperation s when IsLayerProperty(s.Value):
                    foreach (var c in s.Cases)
                        foreach (var clause in c.Clauses)
                            if (clause is ISingleValueCaseClauseOperation single)
                                Sink(Role.Index, single.Value, "switch on GameObject.layer");
                            else if (clause is IPatternCaseClauseOperation { Pattern: IConstantPatternOperation pattern })
                                Sink(Role.Index, pattern.Value, "switch on GameObject.layer");
                    break;
                case ISwitchExpressionOperation e when IsLayerProperty(e.Value):
                    foreach (var arm in e.Arms)
                        if (arm.Pattern is IConstantPatternOperation pattern)
                            Sink(Role.Index, pattern.Value, "switch on GameObject.layer");
                    break;
                case IIsPatternOperation p when IsLayerProperty(p.Value) && p.Pattern is IConstantPatternOperation constant:
                    Sink(Role.Index, constant.Value, "GameObject.layer is");
                    break;
            }
        }

        void Sink(Role? role, IOperation? value, string via)
        {
            if (role is Role r && value != null)
                work.Enqueue((value, r, via));
        }

        static string Describe(IOperation target)
        {
            var symbol = Target(target);
            return symbol == null ? "" : $"{symbol.ContainingType?.Name}.{symbol.Name}";
        }

        static bool IsUnity(ISymbol? symbol)
        {
            var ns = symbol?.ContainingNamespace?.ToDisplayString() ?? "";
            return ns == "UnityEngine" || ns.StartsWith("UnityEngine.") || ns == "UnityEditor" || ns.StartsWith("UnityEditor.");
        }

        static bool IsLayerMask(ITypeSymbol? type) =>
            type != null && type.Name == "LayerMask" && IsUnity(type);

        static bool IsIntegral(ITypeSymbol type) => type.SpecialType is
            SpecialType.System_Int32 or SpecialType.System_UInt32 or SpecialType.System_Int64 or
            SpecialType.System_UInt64 or SpecialType.System_Int16 or SpecialType.System_UInt16 or
            SpecialType.System_Byte or SpecialType.System_SByte;

        /// Tables that are not physics layers, though their APIs say "layer".
        static bool OtherTable(ISymbol? symbol)
        {
            var type = symbol?.ContainingType?.ToDisplayString() ?? "";
            var name = symbol?.Name?.ToLowerInvariant() ?? "";
            return type.StartsWith("UnityEngine.AI") || type.Contains("Animator") || type.Contains("Animation")
                || type.StartsWith("UnityEngine.Rendering") || type.Contains("Sorting") || name.Contains("rendering")
                || name.Contains("sorting") || name.Contains("area");
        }

        static bool IsLayerProperty(IOperation op)
        {
            var symbol = Target(op);
            return symbol is IPropertySymbol { Name: "layer" } p && IsUnity(p) && p.ContainingType.Name == "GameObject";
        }

        Role? MemberRole(ISymbol? symbol)
        {
            if (symbol == null)
                return null;
            if (carriers.TryGetValue(symbol, out var known))
                return known;
            if (OtherTable(symbol))
                return null;
            var type = symbol switch
            {
                IPropertySymbol p => p.Type,
                IFieldSymbol f => f.Type,
                _ => null,
            };
            if (type == null)
                return null;
            if (IsLayerMask(type))
                return Role.Mask;
            if (!IsUnity(symbol) || !IsIntegral(type))
                return null;
            var owner = symbol.ContainingType.Name;
            return symbol.Name switch
            {
                "layer" when owner == "GameObject" => Role.Index,
                "cullingMask" or "eventMask" or "layerMask" or "includeLayers" or "excludeLayers" => Role.Mask,
                "value" when owner == "LayerMask" => Role.Mask,
                _ => null,
            };
        }

        Role? ParameterRole(IParameterSymbol parameter)
        {
            if (carriers.TryGetValue(parameter.OriginalDefinition, out var known))
                return known;
            if (parameter.ContainingSymbol is not IMethodSymbol method || OtherTable(method) || OtherTable(parameter))
                return null;
            if (IsLayerMask(parameter.Type))
                return Role.Mask;
            if (!IsUnity(method) || !IsIntegral(parameter.Type))
                return null;
            var owner = method.ContainingType.Name;
            var name = parameter.Name.ToLowerInvariant();
            var physics = owner.StartsWith("Physics") || owner == "LayerMask" || owner == "Camera" || owner == "GameObject";
            if (name.Contains("layermask") || name == "mask" && physics || name == "cullingmask")
                return Role.Mask;
            if (physics && (name == "layer" || name == "layer1" || name == "layer2"))
                return Role.Index;
            return null;
        }

        void Carry(ISymbol symbol, Role role, string via)
        {
            if (carriers.TryGetValue(symbol, out var known))
            {
                if (known != role && mixed.Add(symbol))
                    Emit(symbol.Locations.FirstOrDefault(), symbol.Name, role, null, via, "mixed");
                return;
            }
            // Symbols from compiled libraries have no writes to follow.
            if (!symbol.Locations.Any(l => l.IsInSource))
            {
                return;
            }
            carriers[symbol] = role;
            carrierVia[symbol] = via;
            var through = $"{via} ← {symbol.Name}";
            if (writes.TryGetValue(symbol, out var values))
                foreach (var value in values)
                    work.Enqueue((value, role, through));
            if (arguments.TryGetValue(symbol, out var args))
                foreach (var value in args)
                    work.Enqueue((value, role, through));
            if (returns.TryGetValue(symbol, out var returned))
                foreach (var value in returned)
                    work.Enqueue((value, role, through));
        }

        void Trace(IOperation op, Role role, string via)
        {
            if (!traced.Add((op, role)))
                return;
            var inner = Unwrap(op);
            switch (inner)
            {
                case ILiteralOperation literal:
                    if (literal.ConstantValue.HasValue && Number(literal.ConstantValue.Value) is long value)
                        Emit(literal.Syntax.GetLocation(), literal.Syntax.ToString(), role, value, via, "literal");
                    return;
                case IFieldReferenceOperation { Field: var field } when field.IsConst:
                    Constant(field, inner, role, via);
                    return;
                case IFieldReferenceOperation { Field: var field }:
                    Carry(field, role, via);
                    return;
                case ILocalReferenceOperation { Local: var local } when local.IsConst:
                    Constant(local, inner, role, via);
                    return;
                case ILocalReferenceOperation { Local: var local }:
                    Carry(local, role, via);
                    return;
                case IParameterReferenceOperation { Parameter: var parameter }:
                    Carry(parameter, role, via);
                    return;
                case IPropertyReferenceOperation { Property: var property }:
                    if (property.Locations.Any(l => l.IsInSource))
                        Carry(property, role, via);
                    else if (!IsUnity(property))
                        Emit(inner.Syntax.GetLocation(), inner.Syntax.ToString(), role, null, via, "unknown");
                    // A Unity property read (other.gameObject.layer) is a layer already.
                    return;
                case IInvocationOperation call:
                    {
                        var target = call.TargetMethod;
                        if (IsUnity(target) && target.ContainingType.Name == "LayerMask" && target.Name is "NameToLayer" or "GetMask")
                            return; // by name: never needs to move
                        if (target.Locations.Any(l => l.IsInSource))
                        {
                            Carry(target.OriginalDefinition, role, via);
                            return;
                        }
                        if (IsUnity(target) && target.Name.Contains("Layer"))
                            return; // a layer Unity computed
                        Emit(inner.Syntax.GetLocation(), inner.Syntax.ToString(), role, null, via, "unknown");
                        return;
                    }
                case IBinaryOperation b when b.OperatorKind == BinaryOperatorKind.LeftShift && role == Role.Mask:
                    {
                        var left = Unwrap(b.LeftOperand);
                        if (left.ConstantValue.HasValue && Number(left.ConstantValue.Value) == 1)
                            work.Enqueue((b.RightOperand, Role.Index, via + " (1 << n)"));
                        else
                            Emit(inner.Syntax.GetLocation(), inner.Syntax.ToString(), role, Constant(inner), via, inner.ConstantValue.HasValue ? "computed" : "unknown");
                        return;
                    }
                case IBinaryOperation b when role == Role.Mask && b.OperatorKind is BinaryOperatorKind.Or or BinaryOperatorKind.ExclusiveOr or BinaryOperatorKind.And or BinaryOperatorKind.Add:
                    work.Enqueue((b.LeftOperand, Role.Mask, via));
                    work.Enqueue((b.RightOperand, Role.Mask, via));
                    return;
                case IUnaryOperation u when u.OperatorKind == UnaryOperatorKind.BitwiseNegation && role == Role.Mask:
                    work.Enqueue((u.Operand, Role.Mask, via + " (~)"));
                    return;
                case IUnaryOperation u when u.OperatorKind == UnaryOperatorKind.Plus:
                    work.Enqueue((u.Operand, role, via));
                    return;
                case IConditionalOperation c:
                    work.Enqueue((c.WhenTrue, role, via));
                    if (c.WhenFalse != null)
                        work.Enqueue((c.WhenFalse, role, via));
                    return;
                case ICoalesceOperation c:
                    work.Enqueue((c.Value, role, via));
                    work.Enqueue((c.WhenNull, role, via));
                    return;
                case IDefaultValueOperation:
                    return;
                default:
                    Emit(inner.Syntax.GetLocation(), inner.Syntax.ToString(), role, Constant(inner), via,
                        inner.ConstantValue.HasValue ? "computed" : "unknown");
                    return;
            }
        }

        static long? Constant(IOperation op) =>
            op.ConstantValue.HasValue ? Number(op.ConstantValue.Value) : null;

        static long? Number(object? value) => value switch
        {
            int i => i,
            uint u => u,
            long l => l,
            ulong ul => (long)ul,
            short s => s,
            ushort us => us,
            byte b => b,
            sbyte sb => sb,
            _ => null,
        };

        /// A constant field or local: follows it to the value in its
        /// declaration, which is where the number is written.
        void Constant(ISymbol symbol, IOperation reference, Role role, string via)
        {
            var through = $"{via} ← {symbol.Name}";
            var syntax = symbol.DeclaringSyntaxReferences.FirstOrDefault()?.GetSyntax();
            ExpressionSyntax? value = syntax switch
            {
                VariableDeclaratorSyntax d => d.Initializer?.Value,
                EnumMemberDeclarationSyntax e => e.EqualsValue?.Value,
                _ => null,
            };
            if (syntax == null)
            {
                Emit(reference.Syntax.GetLocation(), reference.Syntax.ToString(), role, Constant(reference), through, "external");
                return;
            }
            if (value == null)
            {
                // An enum member without a value: its number comes from its
                // position, which a rewrite cannot change safely.
                var name = syntax is EnumMemberDeclarationSyntax member ? member.Identifier : default;
                Emit(name.GetLocation(), name.Text, role, Constant(reference), through, "implicit-enum");
                return;
            }
            var model = Model(value.SyntaxTree);
            var op = model.GetOperation(value);
            if (op == null)
            {
                Emit(value.GetLocation(), value.ToString(), role, Constant(reference), through, "computed");
                return;
            }
            work.Enqueue((op, role, through));
        }

        void Emit(Location? location, string text, Role role, long? value, string via, string kind)
        {
            if (location == null || !location.IsInSource)
                return;
            var span = location.GetLineSpan();
            var key = $"{span.Path}:{span.StartLinePosition.Line}:{span.StartLinePosition.Character}:{role}:{kind}";
            if (!seen.Add(key))
                return;
            Uses.Add(new Use
            {
                File = span.Path,
                Line = span.StartLinePosition.Line + 1,
                Column = span.StartLinePosition.Character,
                Text = text.Length > 120 ? text.Substring(0, 120) : text,
                Role = Name(role),
                Value = value,
                Via = via,
                Kind = kind,
            });
        }

        static bool Serialized(IFieldSymbol field)
        {
            if (field.IsStatic || field.IsConst || !field.Locations.Any(l => l.IsInSource))
                return false;
            var attributes = field.GetAttributes().Select(a => a.AttributeClass?.Name ?? "").ToList();
            if (attributes.Contains("NonSerializedAttribute"))
                return false;
            return field.DeclaredAccessibility == Accessibility.Public || attributes.Contains("SerializeField");
        }
    }
}
