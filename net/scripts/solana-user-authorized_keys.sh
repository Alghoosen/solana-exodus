#
# Contains the public keys for users that should automatically be granted access
# to ALL testnets and datacenter nodes.
#
# To add an entry into this list:
# 1. Run: ssh-keygen -t ecdsa -N '' -f ~/.ssh/id-solana-testnet
# 2. Add an entry to SOLANA_USERS with your username
# 3. Add an entry to SOLANA_PUBKEYS with the contents of ~/.ssh/id-solana-testnet.pub
#
# If you need multiple keys with your username, repeatedly add your username to SOLANA_USERS, once per key
#

SOLANA_USERS=()
SOLANA_PUBKEYS=()

SOLANA_USERS+=('mvines')
SOLANA_PUBKEYS+=('ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBFBNwLw0i+rI312gWshojFlNw9NV7WfaKeeUsYADqOvM2o4yrO2pPw+sgW8W+/rPpVyH7zU9WVRgTME8NgFV1Vc=')

SOLANA_USERS+=('sathish')
SOLANA_PUBKEYS+=('ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBGqZAwAZeBl0buOMz4FpUYrtpwk1L5aGKlbd7lI8dpbSx5WVRPWCVKhWzsGMtDUIfmozdzJouk1LPyihghTDgsE=')

SOLANA_USERS+=('carl')
SOLANA_PUBKEYS+=('ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBOk4jgcX/VWSk3j//wXeIynSQjsOt+AjYXM/XZUMa7R1Q8lfIJGK/qHLBP86CMXdpyEKJ5i37QLYOL+0VuRy0CI=')

SOLANA_USERS+=('jack')
SOLANA_PUBKEYS+=('ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEB6YLY4oCfm0e1qPswbzryw0hQEMiVDcUxOwT4bdBbui/ysKGQlVY8bO6vET1Te8EYHz5W4RuPfETbcHmw6dr4=')

SOLANA_USERS+=('trent')
SOLANA_PUBKEYS+=('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEZC/APgZTM1Y/EfNnCHr+BQN+SN4KWfpyGkwMg+nXdC trent@fry')
SOLANA_USERS+=('trent')
SOLANA_PUBKEYS+=('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDgdbzGLiv9vGo3yaJGzxO3Q2/w5TS4Km2sFGQFWGFIJ trent@farnsworth')
SOLANA_USERS+=('trent')
SOLANA_PUBKEYS+=('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOQPwLZKnv/wI1P4JcpkzeBeKDrLGsp+E/I+qFvLigG3 trent@Trents-MBP')

SOLANA_USERS+=('tristan')
SOLANA_PUBKEYS+=('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJ9VNoG7BLPNbyr4YLf3M2LfQycvFclvi/giXvTpLp0b tristan@TristanSolanaMacBook.local')

SOLANA_USERS+=('dan')
SOLANA_PUBKEYS+=('ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBKMl07qHaMCmnvRKBCmahbBAR6GTWkR5BVe8jdzDJ7xzjXLZlf1aqfaOjt5Cu2VxvW7lUtpJQGLJJiMnWuD4Zmc= dan@Dans-MBP.local')

SOLANA_USERS+=('greg')
SOLANA_PUBKEYS+=('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG3eu2c7DZS+FE3MZmtU+nv1nn9RqW0lno0gyKpGtxT7 greg@solana.com')

SOLANA_USERS+=('tyera')
SOLANA_PUBKEYS+=('ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBDSWMrqTMsML19cDKmxhfwkDfMWwpcVSYJ49cYkZYpZfTvFjV/Wdbpklo0+fp98i5AzfNYnvl0oxVpFg8A8dpYk=')

#valverde/sagan
SOLANA_USERS+=('sakridge')
SOLANA_PUBKEYS+=('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIxN1jPgVdSqNmGAjFwA1ypcnME8uM/9NjfaUZBpNdMh sakridge@valverde')
#fermi
SOLANA_USERS+=('sakridge')
SOLANA_PUBKEYS+=('ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILADsMxP8ZtWxpuXjqjMcYpw6d9+4rgdYrmrMEvrLtmd sakridge@fermi.local')

SOLANA_USERS+=('buildkite-agent')
SOLANA_PUBKEYS+=('ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBHnXXGKZF1/qjhsoRp+7Dm124nIgUbGJPFoqlSkagZmGmsqqHlxgosxHhg6ucHerqonqBXtfdmA7QkZoKVzf/yg= buildkite-agent@dumoulin')

SOLANA_USERS+=('pankaj')
SOLANA_PUBKEYS+=('ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAACAQDGeLA5UGYf7UbT9iNYITyI3kPlG1nPP1kOXYbUZnDvJu+96SSYX91GmWI/2lM8farD8aBH7a8xGbmq7VdAh7HuhWyBEatzSDhWqC2gCdDPxdnCfycL6AtMD8Owfe4W/x0Hza37FtD208HbwMbtPYbQWscfWa7VUrclWQwx8+FyRbhj4tqN/12f+0AVjIIxSEFvYBDD7Aqp1+0pbvJvlnBAJBkxxGfcgoJAt9h27cUM/zVuhRFuzPO7XkEwtctYASNXpSlwJydIDbL8yw5bfjhdkpXJD1xsqgl2M+/77lbf5Y1+jUSpewBTZB9oMIxPdOlYTRl0yJR9TgtmmigyUtE5b5xk2dxqtFmFWRSm0FQulC99B5ZVA7NhZhC45tf+6nP+r6FrMLbnKcCWIWZVURidueM3SbkPAM1oy/XJACZcCiS1JW3MRM0vCB0Ka9E0xShENyVmemiDDlYrj7aaGA9sTUkpPqcDqPcd+niKc0E6Xwsj2oz+DbUgOOiAkrdW273f5eOtA0s4yephSFL/jnZIr4RmVDnE+nPT2furFZiT6oZV/IfOL694tCC6H8mr3G4OApTnfh35PKTl5mzea2qL4T8OHMPhCM/ggT6TlFaJN8B3wMKZ0fn6ardzFtIk4lvfMWkmk3pMudgxENVdfx7wg3kuBbdilhoB2cKSpdTgXw== pankaj@solana.com')
