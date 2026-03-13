package main

import (
	"bytes"
	"encoding/json"
	"go/ast"
	"go/format"
	"go/parser"
	"go/token"
	"go/types"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/tools/go/packages"
)

type helperRequest struct {
	WorkspaceRoot string   `json:"workspace_root"`
	ChangedFiles  []string `json:"changed_files"`
}

type helperResponse struct {
	Packages []packageSnapshot `json:"packages"`
	Files    []fileSnapshot    `json:"files"`
}

type packageSnapshot struct {
	Dir             string            `json:"dir"`
	Exports         map[string]string `json:"exports"`
	Implementations []string          `json:"implementations"`
}

type fileSnapshot struct {
	Path       string `json:"path"`
	Goroutines uint32 `json:"goroutines"`
	Defers     uint32 `json:"defers"`
	Selects    uint32 `json:"selects"`
	Sends      uint32 `json:"sends"`
	Receives   uint32 `json:"receives"`
	Closes     uint32 `json:"closes"`
	MaxNesting uint32 `json:"max_nesting"`
	ErrorsIsAs uint32 `json:"errors_is_as_calls"`
	NilChecks  uint32 `json:"nil_checks"`
	PanicCalls uint32 `json:"panic_calls"`
	Recovers   uint32 `json:"recover_calls"`
	ContextOps uint32 `json:"context_checks"`
	TimeCalls  uint32            `json:"time_calls"`
	RetryHints uint32            `json:"retry_markers"`
	Receivers  map[string]string `json:"receiver_kinds"`
	CleanupOps uint32            `json:"cleanup_calls"`
}

func main() {
	var request helperRequest
	if err := json.NewDecoder(os.Stdin).Decode(&request); err != nil {
		fail(err)
	}

	dirs := changedDirs(request.ChangedFiles)
	response := helperResponse{
		Packages: []packageSnapshot{},
		Files:    []fileSnapshot{},
	}
	for _, dir := range dirs {
		pkgSnapshot, fileSnapshots, err := analyzePackage(request.WorkspaceRoot, dir, request.ChangedFiles)
		if err != nil {
			fail(err)
		}
		response.Packages = append(response.Packages, pkgSnapshot)
		response.Files = append(response.Files, fileSnapshots...)
	}

	if err := json.NewEncoder(os.Stdout).Encode(response); err != nil {
		fail(err)
	}
}

func changedDirs(paths []string) []string {
	set := map[string]struct{}{}
	for _, path := range paths {
		dir := filepath.Dir(path)
		if dir == "." || dir == "/" {
			dir = "."
		}
		set[dir] = struct{}{}
	}

	dirs := make([]string, 0, len(set))
	for dir := range set {
		dirs = append(dirs, dir)
	}
	sort.Strings(dirs)
	return dirs
}

func analyzePackage(workspaceRoot, dir string, changedFiles []string) (packageSnapshot, []fileSnapshot, error) {
	packageDir := filepath.Join(workspaceRoot, dir)
	fset := token.NewFileSet()
	pkgs, err := parser.ParseDir(fset, packageDir, func(info os.FileInfo) bool {
		return strings.HasSuffix(info.Name(), ".go") && !strings.HasSuffix(info.Name(), "_test.go")
	}, parser.SkipObjectResolution)
	if err != nil {
		return packageSnapshot{}, nil, err
	}
	if len(pkgs) == 0 {
		return packageSnapshot{Dir: dir, Exports: map[string]string{}, Implementations: []string{}}, []fileSnapshot{}, nil
	}

	var target *ast.Package
	for _, pkg := range pkgs {
		target = pkg
		break
	}

	files := make([]*ast.File, 0, len(target.Files))
	fileList := make([]*ast.File, 0, len(target.Files))
	fileNames := make([]string, 0, len(target.Files))
	for filename, file := range target.Files {
		files = append(files, file)
		fileList = append(fileList, file)
		fileNames = append(fileNames, filename)
	}

	snapshot, err := typedPackageSnapshot(workspaceRoot, dir)
	if err != nil {
		snapshot = packageSnapshot{
			Dir:             dir,
			Exports:         exportedObjects(target),
			Implementations: implementationMatrix(target),
		}
	}

	changedSet := map[string]struct{}{}
	for _, path := range changedFiles {
		changedSet[filepath.Clean(filepath.Join(workspaceRoot, path))] = struct{}{}
	}

	fileSnapshots := make([]fileSnapshot, 0, len(fileList))
	for _, file := range fileList {
		position := fset.Position(file.Package)
		filename := filepath.Clean(position.Filename)
		if _, ok := changedSet[filename]; !ok {
			continue
		}
		fileSnapshots = append(fileSnapshots, snapshotFile(workspaceRoot, filename, file))
	}

	return snapshot, fileSnapshots, nil
}

func typedPackageSnapshot(workspaceRoot, dir string) (packageSnapshot, error) {
	cfg := &packages.Config{
		Mode: packages.NeedName |
			packages.NeedFiles |
			packages.NeedCompiledGoFiles |
			packages.NeedTypes |
			packages.NeedTypesInfo |
			packages.NeedSyntax |
			packages.NeedModule,
		Dir: filepath.Join(workspaceRoot, dir),
		Env: os.Environ(),
	}

	pkgs, err := packages.Load(cfg, ".")
	if err != nil {
		return packageSnapshot{}, err
	}
	if packages.PrintErrors(pkgs) > 0 || len(pkgs) == 0 || pkgs[0].Types == nil {
		return packageSnapshot{}, io.ErrUnexpectedEOF
	}

	return packageSnapshot{
		Dir:             dir,
		Exports:         typedExports(pkgs[0].Types.Scope()),
		Implementations: typedImplementationMatrix(pkgs[0].Types.Scope()),
	}, nil
}

func typedExports(scope *types.Scope) map[string]string {
	exports := map[string]string{}
	for _, name := range scope.Names() {
		object := scope.Lookup(name)
		switch typed := object.(type) {
		case *types.Func:
			if typed.Exported() {
				exports[name] = types.TypeString(typed.Type(), qualifier)
			}
		case *types.TypeName:
			if typed.Exported() {
				exports[name] = types.TypeString(typed.Type(), qualifier)
			}
		}
	}
	return exports
}

func typedImplementationMatrix(scope *types.Scope) []string {
	interfaces := map[string]*types.Interface{}
	concretes := map[string]*types.Named{}

	for _, name := range scope.Names() {
		typeName, ok := scope.Lookup(name).(*types.TypeName)
		if !ok {
			continue
		}
		named, ok := typeName.Type().(*types.Named)
		if !ok {
			continue
		}
		if iface, ok := named.Underlying().(*types.Interface); ok {
			interfaces[name] = iface.Complete()
			continue
		}
		concretes[name] = named
	}

	implementations := []string{}
	for concreteName, concrete := range concretes {
		for ifaceName, iface := range interfaces {
			if concreteName == ifaceName {
				continue
			}
			if types.Implements(concrete, iface) {
				implementations = append(implementations, concreteName+"=>"+ifaceName)
			}
			pointerType := types.NewPointer(concrete)
			if types.Implements(pointerType, iface) {
				implementations = append(implementations, "*"+concreteName+"=>"+ifaceName)
			}
		}
	}

	sort.Strings(implementations)
	return implementations
}

func qualifier(pkg *types.Package) string {
	if pkg == nil {
		return ""
	}
	return pkg.Path()
}

func exportedObjects(pkg *ast.Package) map[string]string {
	exports := map[string]string{}

	for _, file := range pkg.Files {
		for _, decl := range file.Decls {
			switch typed := decl.(type) {
			case *ast.FuncDecl:
				if typed.Recv != nil || typed.Name == nil || !typed.Name.IsExported() {
					continue
				}
				exports[typed.Name.Name] = funcSignature(typed)
			case *ast.GenDecl:
				if typed.Tok != token.TYPE {
					continue
				}
				for _, spec := range typed.Specs {
					typeSpec, ok := spec.(*ast.TypeSpec)
					if !ok || !typeSpec.Name.IsExported() {
						continue
					}
					exports[typeSpec.Name.Name] = typeSignature(typeSpec)
				}
			}
		}
	}
	return exports
}

func implementationMatrix(pkg *ast.Package) []string {
	interfaces := collectInterfaces(pkg)
	methodSets := collectMethodSets(pkg)
	implementations := []string{}

	for typeName, methods := range methodSets.value {
		for ifaceName, ifaceMethods := range interfaces {
			if typeName == ifaceName {
				continue
			}
			if implements(methods, ifaceMethods) {
				implementations = append(implementations, typeName+"=>"+ifaceName)
			}
		}
	}

	for typeName, methods := range methodSets.pointer {
		for ifaceName, ifaceMethods := range interfaces {
			if typeName == ifaceName {
				continue
			}
			if implements(methods, ifaceMethods) {
				implementations = append(implementations, "*"+typeName+"=>"+ifaceName)
			}
		}
	}

	sort.Strings(implementations)
	return implementations
}

type methodCollection struct {
	value   map[string]map[string]string
	pointer map[string]map[string]string
}

func collectInterfaces(pkg *ast.Package) map[string]map[string]string {
	result := map[string]map[string]string{}

	for _, file := range pkg.Files {
		for _, decl := range file.Decls {
			genDecl, ok := decl.(*ast.GenDecl)
			if !ok || genDecl.Tok != token.TYPE {
				continue
			}
			for _, spec := range genDecl.Specs {
				typeSpec, ok := spec.(*ast.TypeSpec)
				if !ok {
					continue
				}
				iface, ok := typeSpec.Type.(*ast.InterfaceType)
				if !ok {
					continue
				}
				methods := map[string]string{}
				for _, field := range iface.Methods.List {
					if len(field.Names) == 0 {
						continue
					}
					for _, name := range field.Names {
						if fn, ok := field.Type.(*ast.FuncType); ok {
							methods[name.Name] = name.Name + signatureString(fn)
							continue
						}
						methods[name.Name] = name.Name + " " + exprString(field.Type)
					}
				}
				result[typeSpec.Name.Name] = methods
			}
		}
	}

	return result
}

func collectMethodSets(pkg *ast.Package) methodCollection {
	value := map[string]map[string]string{}
	pointer := map[string]map[string]string{}

	for _, file := range pkg.Files {
		for _, decl := range file.Decls {
			funcDecl, ok := decl.(*ast.FuncDecl)
			if !ok || funcDecl.Recv == nil || len(funcDecl.Recv.List) == 0 {
				continue
			}

			typeName, pointerRecv := receiverTypeName(funcDecl.Recv.List[0].Type)
			if typeName == "" {
				continue
			}

			if _, ok := value[typeName]; !ok {
				value[typeName] = map[string]string{}
			}
			if _, ok := pointer[typeName]; !ok {
				pointer[typeName] = map[string]string{}
			}

			signature := methodSignature(funcDecl)
			value[typeName][funcDecl.Name.Name] = signature
			pointer[typeName][funcDecl.Name.Name] = signature
			if pointerRecv {
				delete(value[typeName], funcDecl.Name.Name)
			}
		}
	}

	return methodCollection{value: value, pointer: pointer}
}

func implements(methods map[string]string, ifaceMethods map[string]string) bool {
	for name, signature := range ifaceMethods {
		if methods[name] != signature {
			return false
		}
	}
	return true
}

func receiverTypeName(expr ast.Expr) (string, bool) {
	switch typed := expr.(type) {
	case *ast.Ident:
		return typed.Name, false
	case *ast.StarExpr:
		if ident, ok := typed.X.(*ast.Ident); ok {
			return ident.Name, true
		}
	}
	return "", false
}

func funcSignature(decl *ast.FuncDecl) string {
	return "func " + decl.Name.Name + signatureString(decl.Type)
}

func methodSignature(decl *ast.FuncDecl) string {
	return decl.Name.Name + signatureString(decl.Type)
}

func typeSignature(spec *ast.TypeSpec) string {
	return "type " + spec.Name.Name + " " + exprString(spec.Type)
}

func signatureString(fn *ast.FuncType) string {
	var builder strings.Builder
	builder.WriteString("(")
	builder.WriteString(fieldListString(fn.Params, false))
	builder.WriteString(")")
	if fn.Results != nil {
		results := fieldListString(fn.Results, false)
		if fn.Results.NumFields() == 1 && len(fn.Results.List[0].Names) == 0 {
			builder.WriteString(" ")
			builder.WriteString(results)
		} else {
			builder.WriteString(" (")
			builder.WriteString(results)
			builder.WriteString(")")
		}
	}
	return builder.String()
}

func fieldListString(list *ast.FieldList, includeNames bool) string {
	if list == nil || len(list.List) == 0 {
		return ""
	}

	parts := make([]string, 0, len(list.List))
	for _, field := range list.List {
		typeString := exprString(field.Type)
		if len(field.Names) == 0 || !includeNames {
			parts = append(parts, typeString)
			continue
		}

		names := make([]string, 0, len(field.Names))
		for _, name := range field.Names {
			names = append(names, name.Name)
		}
		parts = append(parts, strings.Join(names, ", ")+" "+typeString)
	}

	return strings.Join(parts, ", ")
}

func exprString(expr ast.Expr) string {
	var buffer bytes.Buffer
	if err := format.Node(&buffer, token.NewFileSet(), expr); err != nil {
		return ""
	}
	return buffer.String()
}

func snapshotFile(workspaceRoot, filename string, file *ast.File) fileSnapshot {
	snapshot := fileSnapshot{
		Path:      relativePath(workspaceRoot, filename),
		Receivers: map[string]string{},
	}

	ast.Inspect(file, func(node ast.Node) bool {
		switch typed := node.(type) {
		case *ast.FuncDecl:
			if typed.Recv != nil && len(typed.Recv.List) > 0 && typed.Name != nil {
				typeName, pointerRecv := receiverTypeName(typed.Recv.List[0].Type)
				if typeName != "" {
					kind := "value"
					if pointerRecv {
						kind = "pointer"
					}
					snapshot.Receivers[typeName+"."+typed.Name.Name] = kind
				}
			}
		case *ast.GoStmt:
			snapshot.Goroutines++
		case *ast.DeferStmt:
			snapshot.Defers++
			if isCleanupCall(typed.Call) {
				snapshot.CleanupOps++
			}
		case *ast.SelectStmt:
			snapshot.Selects++
		case *ast.SendStmt:
			snapshot.Sends++
		case *ast.UnaryExpr:
			if typed.Op == token.ARROW {
				snapshot.Receives++
			}
		case *ast.CallExpr:
			switch fun := typed.Fun.(type) {
			case *ast.Ident:
				switch fun.Name {
				case "close":
					snapshot.Closes++
				case "panic":
					snapshot.PanicCalls++
				case "recover":
					snapshot.Recovers++
				}
			case *ast.SelectorExpr:
				if ident, ok := fun.X.(*ast.Ident); ok && ident.Name == "errors" {
					if fun.Sel.Name == "Is" || fun.Sel.Name == "As" {
						snapshot.ErrorsIsAs++
					}
				}
				if ident, ok := fun.X.(*ast.Ident); ok && ident.Name == "context" {
					if fun.Sel.Name == "Canceled" || fun.Sel.Name == "DeadlineExceeded" {
						snapshot.ContextOps++
					}
				}
				if ident, ok := fun.X.(*ast.Ident); ok && ident.Name == "time" {
					switch fun.Sel.Name {
					case "After", "Sleep", "NewTimer", "NewTicker", "Tick", "AfterFunc":
						snapshot.TimeCalls++
					}
				}
			}
		case *ast.BinaryExpr:
			if typed.Op == token.EQL || typed.Op == token.NEQ {
				if isNilExpr(typed.X) || isNilExpr(typed.Y) {
					snapshot.NilChecks++
				}
			}
		case *ast.SelectorExpr:
			if typed.Sel.Name == "Done" {
				snapshot.ContextOps++
			}
		case *ast.Ident:
			if typed.Name == "retry" || typed.Name == "retries" || typed.Name == "backoff" {
				snapshot.RetryHints++
			}
		}
		return true
	})
	snapshot.MaxNesting = maxBranchNesting(file)

	return snapshot
}

func maxBranchNesting(file *ast.File) uint32 {
	var maxDepth uint32

	var walk func(ast.Node, uint32)
	walk = func(node ast.Node, depth uint32) {
		if node == nil {
			return
		}

		nextDepth := depth
		switch node.(type) {
		case *ast.IfStmt, *ast.ForStmt, *ast.RangeStmt, *ast.SwitchStmt, *ast.TypeSwitchStmt, *ast.SelectStmt:
			nextDepth = depth + 1
			if nextDepth > maxDepth {
				maxDepth = nextDepth
			}
		}

		ast.Inspect(node, func(child ast.Node) bool {
			if child == nil || child == node {
				return true
			}
			walk(child, nextDepth)
			return false
		})
	}

	for _, decl := range file.Decls {
		walk(decl, 0)
	}

	return maxDepth
}

func isNilExpr(expr ast.Expr) bool {
	ident, ok := expr.(*ast.Ident)
	return ok && ident.Name == "nil"
}

func isCleanupCall(call *ast.CallExpr) bool {
	if call == nil {
		return false
	}

	switch fun := call.Fun.(type) {
	case *ast.Ident:
		switch fun.Name {
		case "cancel", "close", "unlock":
			return true
		}
	case *ast.SelectorExpr:
		switch fun.Sel.Name {
		case "Close", "Rollback", "Commit", "Unlock", "Release", "Stop", "Cancel":
			return true
		}
	}

	return false
}

func relativePath(workspaceRoot, filename string) string {
	relative, err := filepath.Rel(workspaceRoot, filename)
	if err != nil {
		return filename
	}
	return filepath.ToSlash(relative)
}

func fail(err error) {
	_, _ = io.WriteString(os.Stderr, err.Error())
	os.Exit(1)
}
