let cargo_packages =  {
  "packages": [
    { "package": "emacs-lsp-booster",
      "git_remote": "https://github.com/blahgeek/emacs-lsp-booster", # if it is installed from a git repo
      "no_default_features": false, #override features array 
      "all_features": true, #overrides features array and no_default_features
      "features": [],
      "post_hook": {|| echo foo},
    },
  ]
}
