# The login is a display label; only the immutable account ID grants access.
{ lib }:
lib.types.submodule {
  options = {
    login = lib.mkOption {
      type = lib.types.str;
      description = "GitHub login, used only in messages.";
    };
    id = lib.mkOption {
      type = lib.types.ints.positive;
      description = "Numeric GitHub account ID (REST user.id or GraphQL databaseId), never an App ID.";
    };
  };
}
