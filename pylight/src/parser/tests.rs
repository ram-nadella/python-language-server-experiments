#[cfg(test)]
mod parser_tests {
    use crate::{PythonParser, SymbolKind};
    use std::path::Path;

    #[test]
    fn test_parser_creation() {
        let parser = PythonParser::new();
        assert!(parser.is_ok());
    }

    #[test]
    fn test_parse_empty_file() {
        let mut parser = PythonParser::new().unwrap();
        let symbols = parser.parse_file(Path::new("empty.py"), "").unwrap();
        assert_eq!(symbols.len(), 0);
    }

    #[test]
    fn test_parse_simple_function() {
        let mut parser = PythonParser::new().unwrap();
        let code = "def hello():\n    pass";
        let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
        assert_eq!(symbols[0].kind, SymbolKind::Function);
        assert_eq!(symbols[0].line, 1);
    }

    #[test]
    fn test_parse_simple_class() {
        let mut parser = PythonParser::new().unwrap();
        let code = "class MyClass:\n    pass";
        let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MyClass");
        assert_eq!(symbols[0].kind, SymbolKind::Class);
        assert_eq!(symbols[0].line, 1);
    }

    #[test]
    fn test_parse_method() {
        let mut parser = PythonParser::new().unwrap();
        let code = r#"
class MyClass:
    def my_method(self):
        pass
"#;
        let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();

        assert_eq!(symbols.len(), 2);

        let class_symbol = &symbols[0];
        assert_eq!(class_symbol.name, "MyClass");
        assert_eq!(class_symbol.kind, SymbolKind::Class);

        let method_symbol = &symbols[1];
        assert_eq!(method_symbol.name, "my_method");
        assert_eq!(method_symbol.kind, SymbolKind::Method);
        assert_eq!(method_symbol.container_name.as_deref(), Some("MyClass"));
    }

    #[test]
    fn test_parse_nested_function() {
        let mut parser = PythonParser::new().unwrap();
        let code = r#"
def outer():
    def inner():
        pass
"#;
        let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();

        assert_eq!(symbols.len(), 2);

        let outer_symbol = &symbols[0];
        assert_eq!(outer_symbol.name, "outer");
        assert_eq!(outer_symbol.kind, SymbolKind::Function);

        let inner_symbol = &symbols[1];
        assert_eq!(inner_symbol.name, "inner");
        assert_eq!(inner_symbol.kind, SymbolKind::NestedFunction);
        assert_eq!(inner_symbol.container_name.as_deref(), Some("outer"));
    }

    #[test]
    fn test_parse_decorated_function() {
        let mut parser = PythonParser::new().unwrap();
        let code = r#"
@decorator
def decorated():
    pass
"#;
        let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "decorated");
        assert_eq!(symbols[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_parse_async_function() {
        let mut parser = PythonParser::new().unwrap();
        let code = "async def async_func():\n    pass";
        let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "async_func");
        assert_eq!(symbols[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_parse_property() {
        let mut parser = PythonParser::new().unwrap();
        let code = r#"
class MyClass:
    @property
    def value(self):
        return self._value
"#;
        let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();

        assert!(symbols
            .iter()
            .any(|s| s.name == "value" && s.kind == SymbolKind::Method));
    }

    #[test]
    fn test_column_positions() {
        use crate::parser::{create_parser, ParserBackend};

        // Test both parser backends
        for backend in [ParserBackend::TreeSitter, ParserBackend::Ruff] {
            let parser = create_parser(backend).unwrap();

            // Test function column position
            let code = "def my_func():\n    pass";
            let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();
            assert_eq!(symbols.len(), 1);
            assert_eq!(symbols[0].name, "my_func");
            assert_eq!(symbols[0].line, 1);
            assert_eq!(
                symbols[0].column, 4,
                "Function name should start at column 4 (0-based) for parser {:?}",
                backend
            );

            // Test class column position
            let code = "class MyClass:\n    pass";
            let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();
            assert_eq!(symbols.len(), 1);
            assert_eq!(symbols[0].name, "MyClass");
            assert_eq!(symbols[0].line, 1);
            assert_eq!(
                symbols[0].column, 6,
                "Class name should start at column 6 (0-based) for parser {:?}",
                backend
            );

            // Test indented method column position
            let code = "class MyClass:\n    def my_method(self):\n        pass";
            let symbols = parser.parse_file(Path::new("test.py"), code).unwrap();
            let method = symbols.iter().find(|s| s.name == "my_method").unwrap();
            assert_eq!(method.line, 2);
            assert_eq!(
                method.column, 8,
                "Method name should start at column 8 (0-based) for parser {:?}",
                backend
            );
        }
    }
}
