; survol index query for JavaScript and JSX (tree-sitter-javascript).
; Same capture conventions as java.scm. When several patterns capture the same
; node, the first pattern in this file wins.

(import_statement) @import
(export_statement source: (_)) @import
(call_expression function: (identifier) @_f arguments: (arguments . (string)) (#eq? @_f "require")) @import

; ---- definitions
(class_declaration name: (identifier) @name) @definition.class
(method_definition name: [(property_identifier) (private_property_identifier)] @name) @definition.method
(function_declaration name: (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function
(field_definition property: [(property_identifier) (private_property_identifier)] @name value: [(arrow_function) (function_expression)]) @definition.method
(field_definition property: [(property_identifier) (private_property_identifier)] @name) @definition.field

; ---- references
(call_expression function: [(identifier) @name
                            (member_expression object: (_) @receiver property: [(property_identifier) (private_property_identifier)] @name)]) @reference.call
(new_expression constructor: [(identifier) @name
                               (member_expression property: (property_identifier) @name)]) @reference.new
(class_heritage (_) @name) @reference.extends

; ---- bindings inferred from `new`
(field_definition property: (property_identifier) @binding.name value: (new_expression constructor: (identifier) @binding.type)) @binding.field
(variable_declarator name: (identifier) @binding.name value: (new_expression constructor: (identifier) @binding.type)) @binding.local
