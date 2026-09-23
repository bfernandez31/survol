; survol index query for TypeScript and TSX (tree-sitter-typescript).
; Same capture conventions as java.scm. When several patterns capture the same
; node, the first pattern in this file wins.

(import_statement) @import
(export_statement source: (_)) @import
(call_expression function: (identifier) @_f arguments: (arguments . (string)) (#eq? @_f "require")) @import

; ---- definitions
(class_declaration name: (type_identifier) @name) @definition.class
(abstract_class_declaration name: (type_identifier) @name) @definition.class
(interface_declaration name: (type_identifier) @name) @definition.interface
(enum_declaration name: (identifier) @name) @definition.enum
(method_definition name: [(property_identifier) (private_property_identifier)] @name) @definition.method
(method_signature name: (property_identifier) @name) @definition.method
(abstract_method_signature name: (property_identifier) @name) @definition.method
(function_declaration name: (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function
(public_field_definition name: [(property_identifier) (private_property_identifier)] @name value: [(arrow_function) (function_expression)]) @definition.method
(public_field_definition name: [(property_identifier) (private_property_identifier)] @name) @definition.field

; ---- references
(call_expression function: [(identifier) @name
                            (member_expression object: (_) @receiver property: [(property_identifier) (private_property_identifier)] @name)]) @reference.call
(new_expression constructor: [(identifier) @name
                               (member_expression property: (property_identifier) @name)]) @reference.new
(extends_clause value: (_) @name) @reference.extends
(implements_clause (_) @name) @reference.implements
(extends_type_clause type: (_) @name) @reference.extends
(type_identifier) @name @reference.type

; ---- typed bindings
(required_parameter (accessibility_modifier) pattern: (identifier) @binding.name type: (type_annotation (_) @binding.type)) @binding.field
(required_parameter pattern: (identifier) @binding.name type: (type_annotation (_) @binding.type)) @binding.local
(optional_parameter pattern: (identifier) @binding.name type: (type_annotation (_) @binding.type)) @binding.local
(public_field_definition name: (property_identifier) @binding.name type: (type_annotation (_) @binding.type)) @binding.field
(public_field_definition name: (property_identifier) @binding.name value: (new_expression constructor: (identifier) @binding.type)) @binding.field
(public_field_definition name: (property_identifier) @binding.name
  value: (call_expression function: (identifier) @_f arguments: (arguments . (identifier) @binding.type)) (#eq? @_f "inject")) @binding.field
(variable_declarator name: (identifier) @binding.name type: (type_annotation (_) @binding.type)) @binding.local
(variable_declarator name: (identifier) @binding.name value: (new_expression constructor: (identifier) @binding.type)) @binding.local
(variable_declarator name: (identifier) @binding.name
  value: (call_expression function: (identifier) @_f arguments: (arguments . (identifier) @binding.type)) (#eq? @_f "inject")) @binding.local
