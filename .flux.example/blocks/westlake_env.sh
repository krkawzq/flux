# ---------- 环境变量 ----------
# 保持缓存路径不变
export UV_CACHE_DIR=/zhoujingbo/.uv-cache
export UV_PYTHON_INSTALL_DIR=/zhoujingbo/.uv-python
export HF_HOME=/zhoujingbo/.cache/huggingface

# 1. Hugging Face 镜像（保持 hf-mirror，这是目前国内最稳定的常规选择）
export HF_ENDPOINT=https://hf-mirror.com

# 2. Python 镜像 (改为阿里云或清华大学，覆盖 UV 和 PIP)
# 阿里云：https://mirrors.aliyun.com/pypi/simple/
# 清华源：https://pypi.tuna.tsinghua.edu.cn/simple
export UV_INDEX_URL=https://mirrors.aliyun.com/pypi/simple
export PIP_INDEX_URL=https://mirrors.aliyun.com/pypi/simple

# 3. NPM 镜像 (使用淘宝/阿里镜像站，这是最常规的国内源)
export NPM_CONFIG_REGISTRY=https://registry.npmmirror.com

# 4. Weights & Biases (WandB) 
# 常规用法通常直接访问官方。如果你在内网环境，建议取消自定义 URL 恢复默认
# 若想恢复官方地址，可以直接注释掉或删掉这一行
unset WANDB_BASE_URL 

# 5. 系统路径
export PATH="/root/.local/bin:$PATH"